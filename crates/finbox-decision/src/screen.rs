//! 全市场初筛：硬过滤 + 多因子打分，只输出少量精品候选。
//!
//! 设计目标：少而精。LLM 面对 3-5 只精选比 40 只噪声更容易选对。
//!
//! 打分因子全部由单条 SQL（`market_screen_rows`）计算，避免逐只查询 10M 行日K表。
//!
//! 硬过滤（一票否决）：
//! - 非 ST / 退市风险
//! - 流动性：当日成交额 ≥ 3000 万
//! - 非涨停（涨幅 < 9.5%）
//!
//! 打分因子（0~1，加权）：
//! - 趋势：close > MA20 且 MA20 > MA60（多头排列）
//! - 回调：今日涨幅温和（0~7%），近 5 日涨幅 -8%~+15%
//! - 放量：量比 1~3（温和放量，非巨量）
//! - 位置：60 日高低点的 20%~80% 分位

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
    /// 打分（0~1），用于展示
    pub score: f64,
}

/// 过滤阈值。
const MIN_TURNOVER: f64 = 30_000_000.0; // 成交额 ≥ 3000 万
const MAX_PCT_LIMIT: f64 = 9.5; // 非涨停（主板）

/// 初筛：硬过滤 + 打分，输出分数最高的 `count` 只。
pub fn screen(db: &Db, count: usize) -> finbox_store::Result<Vec<Candidate>> {
    let rows = db.market_screen_rows()?;
    let mut scored: Vec<(f64, &ScreenRow)> = Vec::new();

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

        // ---- 打分 ----
        let score = score_symbol(s);
        if score > 0.0 {
            scored.push((score, s));
        }
    }

    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    let mut out = Vec::new();
    for (score, s) in scored.iter().take(count) {
        out.push(Candidate {
            thscode: s.thscode.clone(),
            name: ticker_name(db, &s.thscode),
            price: s.price,
            pct: s.pct,
            volume_ratio: s.volume_ratio,
            reason: format!("综合评分 {score:.2}"),
            score: *score,
        });
    }
    Ok(out)
}

/// 多因子打分（0~1）。
fn score_symbol(s: &ScreenRow) -> f64 {
    let mut score = 0.0;

    // 趋势因子（0.3）：多头排列
    if s.ma20 > 0.0 && s.ma60 > 0.0 {
        if s.price > s.ma20 && s.ma20 > s.ma60 {
            score += 0.3;
        } else if s.price > s.ma20 {
            score += 0.15;
        }
    }

    // 回调/强度因子（0.2）：5 日涨幅 -8%~+15%（有动量不追高）
    if s.chg5 > -8.0 && s.chg5 < 15.0 {
        score += 0.2;
    } else if s.chg5 >= 15.0 && s.chg5 < 25.0 {
        score += 0.1;
    }

    // 当日涨幅因子（0.2）：0~7% 温和
    if s.pct >= 0.0 && s.pct < 7.0 {
        score += 0.2;
    } else if s.pct >= 7.0 && s.pct < 9.5 {
        score += 0.1;
    }

    // 放量因子（0.15）：量比 1~3 温和放量
    if let Some(vr) = s.volume_ratio {
        if vr >= 1.0 && vr <= 3.0 {
            score += 0.15;
        } else if vr > 3.0 && vr <= 5.0 {
            score += 0.08;
        }
    }

    // 位置因子（0.15）：60 日高低点的 20%~80% 分位
    if let Some(pos) = s.position {
        if pos >= 0.2 && pos <= 0.8 {
            score += 0.15;
        } else if pos > 0.8 && pos < 0.95 {
            score += 0.05;
        }
    }

    score
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
        }
    }

    #[test]
    fn uptrend_scores_high() {
        // 多头排列 + 温和涨幅 + 温和放量 + 位置适中 → 高分
        let s = row(2.0, 5.0, Some(1.5), 15.0, 14.0, 13.0, 0.5);
        let sc = score_symbol(&s);
        assert!(sc > 0.8, "理想票应高分，实际 {sc}");
    }

    #[test]
    fn high_pct_scores_low() {
        let good = row(2.0, 5.0, Some(1.5), 15.0, 14.0, 13.0, 0.5);
        let hot = row(9.0, 20.0, Some(8.0), 15.0, 14.0, 13.0, 0.9);
        assert!(score_symbol(&hot) < score_symbol(&good));
    }

    #[test]
    fn downtrend_scores_low() {
        // 空头排列（价格在 MA 下方）→ 低分
        let s = row(-3.0, -10.0, None, 10.0, 11.0, 12.0, 0.1);
        assert!(score_symbol(&s) < 0.3);
    }
}
