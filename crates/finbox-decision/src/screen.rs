//! 全市场初筛：硬过滤 + 双候选池打分，只输出少量精品候选。
//!
//! 设计目标：少而精。LLM 面对 3-5 只精选比 40 只噪声更容易选对。
//!
//! **双候选池**（修掉"只喂当日强势股 → AI 只能追高"的自相矛盾）：
//! - 强势型：多头排列 + 温和上涨 + 温和放量 + 位置适中（趋势延续）
//! - 回调型：多头排列但缩量回调到 MA20 附近（低吸位置）
//!
//! 打分因子为**连续函数**（三角/线性衰减），不再"满足即满分"——
//! 旧实现所有候选都撞到 1.00，排序失效退化成随机抽样。
//! 最终再按同批 min-max 归一化到 0.3~1.0，保证区分度。
//!
//! 硬过滤（一票否决）：
//! - 非 ST / 退市风险
//! - 流动性：成交额 ≥ 3000 万
//! - 非涨停（涨幅 < 9.5%）
//! - 同一板块最多 2 只（伪分散修正：4 只同板块 = 1 只）

use finbox_store::{Db, ScreenRow};

/// 初筛候选。
#[derive(Debug, Clone)]
pub struct Candidate {
    pub thscode: String,
    pub name: String,
    pub price: f64,
    pub pct: f64,
    pub volume_ratio: Option<f64>,
    pub reason: String,
    /// 打分（0~1，同批归一化后），用于展示
    pub score: f64,
}

/// 候选类型：强势延续 / 回调低吸。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Momentum,
    Pullback,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Kind::Momentum => "强势型",
            Kind::Pullback => "回调型",
        }
    }
}

/// 过滤阈值。
const MIN_TURNOVER: f64 = 30_000_000.0; // 成交额 ≥ 3000 万
const MAX_PCT_LIMIT: f64 = 9.5; // 非涨停（主板）
/// 同一（一级）行业最多候选数
const MAX_PER_INDUSTRY: usize = 2;

/// 初筛：硬过滤 + 双池打分，输出两类型各半的候选。
pub fn screen(db: &Db, count: usize) -> finbox_store::Result<Vec<Candidate>> {
    let rows = db.market_screen_rows()?;
    let mut momentum: Vec<(f64, &ScreenRow)> = Vec::new();
    let mut pullback: Vec<(f64, &ScreenRow)> = Vec::new();

    for s in rows.iter() {
        // ---- 硬过滤 ----
        if s.turnover < MIN_TURNOVER {
            continue; // 流动性不足
        }
        if s.pct >= MAX_PCT_LIMIT {
            continue; // 涨停买不进
        }
        let name = ticker_name(db, &s.thscode);
        if name.contains("ST") || name.contains("退") {
            continue; // ST / 退市风险
        }

        if let Some(sc) = score_momentum(s) {
            momentum.push((sc, s));
        }
        if let Some(sc) = score_pullback(s) {
            pullback.push((sc, s));
        }
    }

    let by_score = |a: &(f64, &ScreenRow), b: &(f64, &ScreenRow)| {
        b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal)
    };
    momentum.sort_by(by_score);
    pullback.sort_by(by_score);

    // 两类各取一半，保证"强势 + 回调"都在池子里（去重：两池条件在 0~2% 区间会重叠）
    let half = (count + 1) / 2;
    let mut picked: Vec<(f64, &ScreenRow, Kind)> = Vec::new();
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for (sc, s) in momentum.iter().take(half) {
        if seen.insert(s.thscode.as_str()) {
            picked.push((*sc, s, Kind::Momentum));
        }
    }
    for (sc, s) in pullback.iter() {
        if picked.len() >= count {
            break;
        }
        if seen.insert(s.thscode.as_str()) {
            picked.push((*sc, s, Kind::Pullback));
        }
    }
    // 某一类不足时用另一类补足到 count
    if picked.len() < count {
        for (sc, s) in momentum.iter().skip(half).chain(pullback.iter()) {
            if picked.len() >= count {
                break;
            }
            if seen.insert(s.thscode.as_str()) {
                picked.push((*sc, s, Kind::Momentum));
            }
        }
    }

    // 同行业去重（伪分散：同行业 4 只 ≈ 1 只）。无行业数据时回退板块名
    let mut ind_count: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    picked.retain(|(_, s, _)| {
        let key = db
            .industry_of(&s.thscode)
            .ok()
            .flatten()
            .unwrap_or_else(|| finbox_core::rules::board_name(&s.thscode).to_string());
        let c = ind_count.entry(key).or_insert(0);
        *c += 1;
        *c <= MAX_PER_INDUSTRY
    });

    // 同批归一化到 0.3~1.0（保证排序区分度；旧实现全是 1.00）
    let (lo, hi) = picked.iter().fold((f64::MAX, f64::MIN), |(lo, hi), (sc, _, _)| {
        (lo.min(*sc), hi.max(*sc))
    });
    let spread = (hi - lo).max(1e-6);

    let out = picked
        .iter()
        .map(|(sc, s, kind)| {
            let norm = 0.3 + 0.7 * (sc - lo) / spread;
            Candidate {
                thscode: s.thscode.clone(),
                name: ticker_name(db, &s.thscode),
                price: s.price,
                pct: s.pct,
                volume_ratio: s.volume_ratio,
                reason: format!("{}({:.2})", kind.label(), norm),
                score: norm,
            }
        })
        .collect();
    Ok(out)
}

/// 三角衰减：x 在 ideal 处得 1，偏离到 tol 处得 0。
fn peak(x: f64, ideal: f64, tol: f64) -> f64 {
    (1.0 - (x - ideal).abs() / tol).clamp(0.0, 1.0)
}

/// 线性爬升：x ≤ lo 得 0，x ≥ hi 得 1。
fn ramp(x: f64, lo: f64, hi: f64) -> f64 {
    if hi <= lo {
        return 0.0;
    }
    ((x - lo) / (hi - lo)).clamp(0.0, 1.0)
}

/// 强势型打分：多头排列 + 温和上涨 + 温和放量 + 位置适中。不满足形态返回 None。
fn score_momentum(s: &ScreenRow) -> Option<f64> {
    if s.ma20 <= 0.0 || s.ma60 <= 0.0 || s.adj_close <= 0.0 {
        return None;
    }
    if !(s.adj_close > s.ma20 && s.ma20 > s.ma60) {
        return None; // 非多头排列
    }
    let mut score = 0.0;
    // 均线发散度（3% 以上给满）
    score += 0.25 * ramp((s.ma20 / s.ma60 - 1.0) * 100.0, 0.0, 4.0);
    // 站上 MA20 的幅度（1% 给满）
    score += 0.20 * ramp((s.adj_close / s.ma20 - 1.0) * 100.0, 0.0, 1.5);
    // 当日涨幅：理想 +2%，±6% 衰减到 0
    score += 0.20 * peak(s.pct, 2.0, 6.0);
    // 量比：理想 1.8，±2.0 衰减到 0
    if let Some(vr) = s.volume_ratio {
        score += 0.20 * peak(vr, 1.8, 2.0);
    } else {
        score += 0.10; // 无量比数据给一半
    }
    // 位置：理想 0.5（60 日区间中部）
    if let Some(pos) = s.position {
        score += 0.15 * peak(pos, 0.5, 0.5);
    }
    Some(score)
}

/// 回调型打分：多头排列但缩量回调到 MA20 附近（低吸位置）。不满足返回 None。
fn score_pullback(s: &ScreenRow) -> Option<f64> {
    if s.ma20 <= 0.0 || s.ma60 <= 0.0 || s.adj_close <= 0.0 {
        return None;
    }
    // 中期仍多头，价格回到 MA20 附近（-3%~+2%）
    if s.ma20 <= s.ma60 {
        return None;
    }
    let dev = (s.adj_close / s.ma20 - 1.0) * 100.0;
    if !(-3.5..=2.0).contains(&dev) {
        return None;
    }
    let mut score = 0.0;
    // 贴近 MA20（理想 -0.5%）
    score += 0.35 * peak(dev, -0.5, 3.0);
    // 当日小跌/横盘（理想 -1%）
    score += 0.20 * peak(s.pct, -1.0, 4.0);
    // 近 5 日小幅回调（理想 -3%）
    score += 0.20 * peak(s.chg5, -3.0, 7.0);
    // 缩量（理想 0.8）
    if let Some(vr) = s.volume_ratio {
        score += 0.15 * peak(vr, 0.8, 0.8);
    }
    // 位置：理想 0.4
    if let Some(pos) = s.position {
        score += 0.10 * peak(pos, 0.4, 0.5);
    }
    Some(score)
}

fn ticker_name(db: &Db, thscode: &str) -> String {
    db.ticker_name(thscode).unwrap_or_else(|_| thscode.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(pct: f64, chg5: f64, vr: Option<f64>, price: f64, ma20: f64, ma60: f64, pos: f64) -> ScreenRow {
        ScreenRow {
            thscode: "600519.SH".into(),
            price,
            pct,
            turnover: 100_000_000.0,
            ma20,
            ma60,
            chg5,
            volume_ratio: vr,
            position: Some(pos),
            adj_close: price,
        }
    }

    #[test]
    fn momentum_scores_high_for_ideal() {
        // 多头排列 + 温和涨幅 + 温和放量 + 位置适中
        let s = row(2.0, 5.0, Some(1.8), 15.0, 14.0, 13.5, 0.5);
        let sc = score_momentum(&s).unwrap();
        assert!(sc > 0.8, "理想强势票应高分，实际 {sc}");
    }

    #[test]
    fn momentum_rejects_non_uptrend() {
        // 空头排列（价格在 MA 下方）→ 不属于强势池
        let s = row(-3.0, -10.0, None, 10.0, 11.0, 12.0, 0.1);
        assert!(score_momentum(&s).is_none());
        assert!(score_pullback(&s).is_none());
    }

    #[test]
    fn hot_stock_scores_lower_than_calm() {
        let good = row(2.0, 5.0, Some(1.8), 15.0, 14.0, 13.5, 0.5);
        let hot = row(9.0, 20.0, Some(8.0), 15.0, 14.0, 13.5, 0.9);
        assert!(score_momentum(&hot).unwrap() < score_momentum(&good).unwrap());
    }

    #[test]
    fn pullback_picks_shrinking_dip() {
        // 多头排列 + 缩量回踩 MA20（dev -0.4%）→ 回调池高分
        let s = row(-1.0, -3.0, Some(0.8), 13.95, 14.0, 13.5, 0.4);
        let sc = score_pullback(&s).unwrap();
        assert!(sc > 0.8, "回踩 MA20 的缩量票应高分，实际 {sc}");
        // 该票不属于强势池（价格在 MA20 下方）
        assert!(score_momentum(&s).is_none());
    }

    #[test]
    fn pullback_rejects_broken_trend() {
        // 跌破 MA20 过多（-5%）→ 不是回调，是走坏
        let s = row(-3.0, -8.0, Some(0.6), 13.3, 14.0, 13.5, 0.2);
        assert!(score_pullback(&s).is_none());
    }
}
