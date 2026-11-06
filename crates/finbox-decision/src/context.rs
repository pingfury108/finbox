//! 构建 LLM 决策上下文：账户、持仓、自选池、候选、趋势摘要、近期复盘。


use crate::screen::Candidate;
use finbox_store::Db;

const SYSTEM_PROMPT: &str = r#"你是一个稳健的 A 股交易员，管理真实资金账户。目标：稳定小赚，严格控制回撤。策略是低风险的日线波段。

核心纪律：
1. 候选池只有 3-5 只，分两类：「强势型」趋势延续、「回调型」缩量回踩 MA20 的低吸位置。你只需从中精选 1-2 只，不要超出候选池
2. 每只股票最大仓位 25%，最多同时持有 4 只；持仓没满时**不要求补位**，宁可空着也不硬买；没有把握就 hold，空仓等待是常态，也是合法决策
3. 买入后按系统风控执行：收盘价跌破成本 -5% 强制止损；盈利 +6% 减 1/3、+10% 再减 1/3。你不必反复交易，持有到目标或止损
4. **换股要有硬理由**：新标的必须明显优于现有持仓（趋势更明确/位置更安全）才换；不要为了填满仓位而卖出尚在上升趋势的持仓。卖出后 60 分钟内系统禁止买入（冷却期），计划换股时把买卖意图分开到不同时段
5. 优先选择：多头排列（MA20>MA60）、回调不破位、温和放量（量比1~3）、离 60 日高点有一定空间
6. 避免：追高（今日涨幅已大）、涨停板（买不进）、ST/退市风险股、下跌趋势
7. 卖出逻辑：持仓盈利到目标、趋势走坏（跌破MA20）、或系统性风险时卖出；也可明确建议减仓/清仓
8. 每次决策都要审视当前持仓：继续持有 / 加仓 / 减仓 / 清仓，给出理由
9. 输出严格 JSON，不要多余内容：
{"actions": [{"action": "buy"|"sell"|"hold", "symbol": "代码", "quantity": 数量, "reason": "理由"}], "comment": "整体判断与市场状态"}
"#;

/// 小额账户（<3 万）：集中、低频、高确信。分散与频繁换手的手续费会吃掉全部利润。
const SYSTEM_PROMPT_COMPACT: &str = r#"你是一个稳健的 A 股交易员，管理一个小资金账户。小资金的生存法则是：**集中、低频、高确信**——手续费和零碎仓位是小资金最大的敌人。

核心纪律：
1. 候选池只有 3-5 只（已过滤你买得起的）。只精选 **1 只**，最多 2 只；没有把握就空仓，空仓是合法且常见的决策
2. 单票最大仓位 55%，最多同时持有 2 只；持仓没满时**不要求补位**，宁可空着也不硬买
3. 每笔交易成本约 0.15%（免 5 佣金），换手一次就亏一次——**只在高确信时出手**，目标是一次赚 6%-10%，不要为了“参与行情”而交易
4. 买入后按系统风控执行：收盘价跌破成本 -5% 强制止损；盈利 +6% 减 1/3、+10% 再减 1/3。不频繁交易，持有到目标或止损
5. **换股要有硬理由**：新标的必须明显优于现有持仓才换；当日卖出后 60 分钟内系统禁止买入
6. 优先选择：多头排列（MA20>MA60）、回调不破位、温和放量（量比1~3）、离 60 日高点有空间
7. 避免：追高（今日涨幅已大）、涨停板（买不进）、ST/退市风险股、下跌趋势
8. 卖出逻辑：盈利到目标、趋势走坏（跌破MA20）、或系统性风险；也可明确建议减仓/清仓
9. 输出严格 JSON，不要多余内容：
{"actions": [{"action": "buy"|"sell"|"hold", "symbol": "代码", "quantity": 数量, "reason": "理由"}], "comment": "整体判断与市场状态"}
"#;

/// 返回系统提示词（按账户规模分档：小额集中打法 / 标准分散打法）。
pub fn system_prompt_for(initial_capital: f64) -> &'static str {
    if initial_capital < 30_000.0 {
        SYSTEM_PROMPT_COMPACT
    } else {
        SYSTEM_PROMPT
    }
}

/// 趋势摘要：近 60 根日 K 的涨跌幅、均线、区间高低点。
fn trend_summary(db: &Db, thscode: &str) -> String {
    let bars = match db.recent_bars(thscode, 60) {
        Ok(b) => b,
        Err(_) => return "历史数据不足".into(),
    };
    if bars.len() < 5 {
        return "历史数据不足".into();
    }
    let cur = bars.last().unwrap().close;
    let n = bars.len();
    let ma20: f64 = bars[n.saturating_sub(20)..].iter().map(|b| b.close).sum::<f64>()
        / bars[n.saturating_sub(20)..].len() as f64;
    let ma60: f64 = bars.iter().map(|b| b.close).sum::<f64>() / n as f64;
    let chg20 = if n > 21 { (cur / bars[n - 22].close - 1.0) * 100.0 } else { 0.0 };
    let hi = bars.iter().map(|b| b.high).fold(f64::MIN, f64::max);
    let lo = bars.iter().map(|b| b.low).fold(f64::MAX, f64::min);
    format!(
        "近20日{chg20:+.1}%, MA20={ma20:.2}, MA60={ma60:.2}, 60日区间 {lo:.2}~{hi:.2}"
    )
}

/// 近 5 日价格序列（形态细节：连阴/放量滞涨等统计摘要看不出的走势）。
fn recent_days_detail(db: &Db, thscode: &str) -> String {
    let bars = match db.recent_bars(thscode, 5) {
        Ok(b) if !b.is_empty() => b,
        _ => return String::new(),
    };
    let parts: Vec<String> = bars
        .iter()
        .map(|b| {
            let d = crate::context::fmt_bar_date(b.date_ms);
            let chg = if b.open > 0.0 { (b.close / b.open - 1.0) * 100.0 } else { 0.0 };
            format!("{}({:+.1}%)", d, chg)
        })
        .collect();
    format!("近5日: {}", parts.join(" "))
}

fn fmt_bar_date(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms + 8 * 3600 * 1000)
        .map(|t| t.format("%m-%d").to_string())
        .unwrap_or_default()
}

/// 大盘环境：主要指数当日涨跌 + 涨跌家数。
fn market_env_summary(market: &finbox_store::SharedDb) -> String {
    let m = market.lock().unwrap();
    let mut parts = Vec::new();
    for (code, name) in [("000001.SH", "上证"), ("399001.SZ", "深成"), ("399006.SZ", "创业板")] {
        if let Ok(Some((price, _chg, pct, _ts))) = m.latest_snapshot_full(code) {
            parts.push(format!("{name} {price:.0}（{pct:+.2}%）"));
        }
    }
    let (up, total) = m.market_breadth().unwrap_or((0, 0));
    if total > 0 {
        parts.push(format!("涨{}家/共{}家", up, total));
    }
    if parts.is_empty() { String::new() } else { parts.join("，") }
}

/// 构建用户上下文文本。
/// 战绩摘要：分桶统计（市场状态 × 候选类型）的 D5 均值与胜率，
/// 按样本量/表现生成结论标注。这是“自我进化”的注入点——
/// 数据每周随真实成交滚动更新，LLM 看到的是自己最近的真实战绩而非静态提示词。
/// 样本不足（<8 笔）的桶不输出，避免噪声误导。
pub fn stats_summary(stats: &[(String, String, i64, f64, f64)]) -> Option<String> {
    let total: i64 = stats.iter().map(|s| s.2).sum();
    if total < 8 {
        return None; // 样本太少，注入反而是噪声
    }
    let mut lines = vec![
        "== 历史战绩（你自己的真实成交统计，D5=买入后5个交易日收益）==".into(),
        "候选类型 × 市场状态 | 样本 | D5均值 | 胜率 | 结论".into(),
    ];
    for (regime, ctype, n, avg, wr) in stats {
        if *n < 8 {
            continue; // 单桶样本不足不下结论
        }
        let verdict = if *avg > 0.5 && *wr >= 55.0 {
            "✓ 表现好，优先此类机会"
        } else if *avg < -0.5 || *wr <= 30.0 {
            "✗ 表现差，此类机会严格回避"
        } else {
            "— 表现一般，正常对待"
        };
        lines.push(format!(
            "{} × {}: {}笔, D5均值{avg:+.2}%, 胜率{wr:.0}% → {verdict}",
            ctype, regime, n
        ));
    }
    lines.push("决策时必须参考上表：优先进入 ✓ 标注的机会，✗ 标注的机会即使看起来诱人也要放弃".into());
    Some(lines.join("\n"))
}

pub fn build_context(
    market: &finbox_store::SharedDb,
    acct: &finbox_store::SharedAccountDb,
    watchlist: &[String],
    candidates: &[Candidate],
) -> finbox_store::Result<String> {
    let account = acct.lock().unwrap().get_or_init_account(0.0)?;
    let positions = acct.lock().unwrap().positions()?;
    let stats = match acct.lock().unwrap().bucket_stats() {
        Ok(v) => v,
        Err(e) => {
            log::warn!("[上下文] 战绩统计读取失败: {e}");
            Vec::new()
        }
    };
    if stats.is_empty() {
        log::info!("[上下文] 战绩统计为空（无已回填的 D5 数据或无候选类型）");
    }

    let mut lines = vec![
        format!("可用现金: {:.2} 元", account.cash),
        String::new(),
    ];
    // 历史战绩注入（自我进化核心：样本足够时才出现）
    if let Some(s) = stats_summary(&stats) {
        lines.push(s);
        lines.push(String::new());
    }
    lines.push("== 大盘环境 ==".into());
    lines.push(market_env_summary(market));
    lines.push(String::new());
    lines.push("== 当前持仓 ==".into());

    if positions.is_empty() {
        lines.push("（空仓）".into());
    } else {
        for p in &positions {
            let cur = market.lock().unwrap().latest_snapshot_price(&p.thscode)?.unwrap_or(0.0);
            let pnl = if cur > 0.0 { format!("{:.2}%", (cur / p.avg_cost - 1.0) * 100.0) } else { "无行情".into() };
            lines.push(format!(
                "{} {}: {}股, 成本 {:.2}, 现价 {:.2}, 盈亏 {}",
                p.thscode, p.name, p.quantity, p.avg_cost, cur, pnl
            ));
        }
    }

    if !watchlist.is_empty() {
        lines.push(String::new());
        lines.push("== 自选池行情与趋势 ==".into());
        for s in watchlist {
            let price = market.lock().unwrap().latest_snapshot_price(s)?.map(|p| format!("{p:.2}")).unwrap_or_else(|| "无数据".into());
            lines.push(format!("{}: 最新价 {} | {}", s, price, trend_summary(&market.lock().unwrap(), s)));
        }
    }

    if !candidates.is_empty() {
        lines.push(String::new());
        lines.push("== 今日全市场初筛候选 ==".into());
        for c in candidates {
            let vr = c.volume_ratio.map(|v| format!("{v:.2}")).unwrap_or_else(|| "-".into());
            let trend = trend_summary(&market.lock().unwrap(), &c.thscode);
            let detail = recent_days_detail(&market.lock().unwrap(), &c.thscode);
            lines.push(format!(
                "{} {}: 现价 {:.2}, 涨幅 {:.2}%, 量比 {} | 入选: {} | {} | {}",
                c.thscode, c.name, c.price, c.pct, vr, c.reason, trend, detail
            ));
        }
    }

    // 近期复盘反馈（你的历史战绩，用于自我修正）
    let reviews = acct.lock().unwrap().recent_reviews(5)?;
    if !reviews.is_empty() {
        lines.push(String::new());
        lines.push("== 近期复盘反馈（你之前决策的验证结果，用于自我修正）==".into());
        for r in reviews {
            lines.push(format!(
                "决策#{} {}天后 盈亏 {:+.0}元 | {}",
                r.decision_id, r.days_after, r.pnl, r.summary
            ));
        }
    }

    Ok(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_summary_requires_min_samples() {
        let few = vec![("risk-on".to_string(), "回调型".to_string(), 3, 1.0, 60.0)];
        assert!(stats_summary(&few).is_none(), "总样本<8 应返回 None");
    }

    #[test]
    fn stats_summary_marks_verdicts() {
        let stats = vec![
            ("risk-on".to_string(), "回调型".to_string(), 23, 1.2, 74.0),
            ("neutral".to_string(), "强势型".to_string(), 8, -4.0, 13.0),
            ("neutral".to_string(), "回调型".to_string(), 5, 0.1, 50.0), // 单桶<8 不输出
        ];
        let s = stats_summary(&stats).unwrap();
        assert!(s.contains("优先此类机会"));
        assert!(s.contains("严格回避"));
        assert!(!s.contains("5笔"), "单桶样本不足不应输出");
        assert!(s.contains("23笔"));
    }
}
