//! 构建 LLM 决策上下文：账户、持仓、自选池、候选、趋势摘要、近期复盘。

use finbox_store::Db;

use crate::screen::Candidate;

const SYSTEM_PROMPT: &str = r#"你是一个 A 股职业交易员，正在管理一个真实资金账户，每一笔交易都是真金白银。根据给出的账户、持仓和全市场初筛候选，决定本轮操作。

规则：
1. 可交易范围 = 当前持仓 + 今日候选（全市场初筛结果） + 自选池（如有）
2. 买卖数量必须是 100 的整数倍
3. 买入金额不能超过可用现金（含佣金等费用）
4. T+1 规则：当天买入的股票当天不能卖出
5. 涨停的股票买不进、跌停的卖不出（主板±10%，创业板/科创板±20%），接近涨跌停的谨慎操作
6. 每次交易有费用（佣金+印花税），频繁倒手会侵蚀收益
7. 候选股重点看：入选原因、量比/换手是否放大、趋势位置；优中选优，不要撒胡椒面
8. 这是真实资金，亏损是真实的：控制风险，不要满仓单只股票，没有把握就 hold，不操作也是合法决策
9. 复盘你的历史决策：被验证错误的判断要总结教训并调整
10. 严格输出 JSON，不要输出其他内容：
{"actions": [{"action": "buy"|"sell"|"hold", "symbol": "代码", "quantity": 数量, "reason": "理由"}], "comment": "整体判断"}
"#;

/// 返回 (system_prompt, user_context)。
pub fn system_prompt() -> &'static str {
    SYSTEM_PROMPT
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

/// 构建用户上下文文本。
pub fn build_context(
    db: &Db,
    watchlist: &[String],
    candidates: &[Candidate],
) -> finbox_store::Result<String> {
    let account = db.get_or_init_account(0.0)?;
    let positions = db.positions()?;

    let mut lines = vec![
        format!("可用现金: {:.2} 元", account.cash),
        String::new(),
        "== 当前持仓 ==".into(),
    ];

    if positions.is_empty() {
        lines.push("（空仓）".into());
    } else {
        for p in &positions {
            let cur = db.latest_snapshot_price(&p.thscode)?.unwrap_or(0.0);
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
            let price = db.latest_snapshot_price(s)?.map(|p| format!("{p:.2}")).unwrap_or_else(|| "无数据".into());
            lines.push(format!("{}: 最新价 {} | {}", s, price, trend_summary(db, s)));
        }
    }

    if !candidates.is_empty() {
        lines.push(String::new());
        lines.push("== 今日全市场初筛候选 ==".into());
        for c in candidates {
            let vr = c.volume_ratio.map(|v| format!("{v:.2}")).unwrap_or_else(|| "-".into());
            lines.push(format!(
                "{} {}: 现价 {:.2}, 涨幅 {:.2}%, 量比 {} | 入选: {} | {}",
                c.thscode, c.name, c.price, c.pct, vr, c.reason, trend_summary(db, &c.thscode)
            ));
        }
    }

    // 近期已执行决策（复盘反馈）
    let recent = db.recent_executed_decisions(5)?;
    if !recent.is_empty() {
        lines.push(String::new());
        lines.push("== 近期决策（你的历史表现，用于自我修正）==".into());
        for d in recent {
            let ts = chrono::DateTime::from_timestamp_millis(d.ts_ms)
                .map(|t| t.format("%m-%d %H:%M").to_string())
                .unwrap_or_default();
            lines.push(format!("[{ts}] {} {}", d.model, d.note));
        }
    }

    Ok(lines.join("\n"))
}
