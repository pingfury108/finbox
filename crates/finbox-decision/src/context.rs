//! 构建 LLM 决策上下文：账户、持仓、自选池、候选、趋势摘要、近期复盘。

use finbox_store::Db;

use crate::screen::Candidate;

const SYSTEM_PROMPT: &str = r#"你是一个稳健的 A 股交易员，管理真实资金账户。目标：稳定小赚，严格控制回撤。策略是低风险的日线波段。

核心纪律：
1. 候选池只有 3-5 只，是系统初筛出的上升趋势、回调到位的股票。你只需从中精选 1-2 只，不要超出候选池
2. 每只股票最大仓位 20%，最多同时持有 3 只；没有把握就 hold，空仓等待是常态，也是合法决策
3. 买入后按系统风控执行：亏损 -5% 强制止损，盈利 +15% 减半止盈。你不必反复交易，持有到目标或止损
4. 优先选择：多头排列（MA20>MA60）、回调不破位、温和放量（量比1~3）、离 60 日高点有一定空间
5. 避免：追高（今日涨幅已大）、涨停板（买不进）、ST/退市风险股、下跌趋势
6. 卖出逻辑：持仓盈利到目标、趋势走坏（跌破MA20）、或系统性风险时卖出；也可明确建议减仓/清仓
7. 每次决策都要审视当前持仓：继续持有 / 加仓 / 减仓 / 清仓，给出理由
8. 输出严格 JSON，不要多余内容：
{"actions": [{"action": "buy"|"sell"|"hold", "symbol": "代码", "quantity": 数量, "reason": "理由"}], "comment": "整体判断与市场状态"}
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

    // 近期复盘反馈（你的历史战绩，用于自我修正）
    let reviews = db.recent_reviews(5)?;
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
