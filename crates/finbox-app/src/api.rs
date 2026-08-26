//! JSON API：供前端 ECharts 与页面 fetch 使用。

use axum::{
    extract::{Path, State},
    Json,
};
use serde::Serialize;

use crate::accounts;
use crate::web::WebState;

/// 日 K 单根（ECharts candlestick 需要 [open, close, low, high] 顺序）。
#[derive(Serialize)]
pub struct KlinePoint {
    pub date: String,
    /// ECharts candlestick 数据：[open, close, low, high]
    pub ohlc: Vec<f64>,
    pub volume: f64,
    pub ma5: f64,
    pub ma10: f64,
    pub ma20: f64,
    pub ma60: f64,
}

/// K 线响应。
#[derive(Serialize)]
pub struct KlineResponse {
    pub thscode: String,
    pub name: String,
    pub points: Vec<KlinePoint>,
}

/// 标的搜索项。
#[derive(Serialize)]
pub struct SymbolItem {
    pub thscode: String,
    pub name: String,
    pub ticker: String,
}

/// 账户资产。
#[derive(Serialize)]
pub struct AccountAsset {
    pub name: String,
    pub cash: f64,
    pub market_value: f64,
    pub total: f64,
    pub return_pct: f64,
    pub position_count: usize,
    /// 今日盈亏（当前真实总市值 - 最近收盘快照总资产）
    pub today_pnl: f64,
    /// 迷你曲线数据（最近资产快照序列）
    pub sparkline: Vec<f64>,
}

/// 指数实时行情（状态条用）。
#[derive(Serialize)]
pub struct IndexQuote {
    pub thscode: String,
    pub name: String,
    pub price: f64,
    pub pct: f64,
}

/// 市场总览（全局状态条）。
#[derive(Serialize)]
pub struct MarketOverview {
    pub indexes: Vec<IndexQuote>,
    /// 上涨家数 / 总家数
    pub up: u32,
    pub total: u32,
    /// 市场状态：risk-on / neutral / risk-off
    pub regime: String,
    /// 最新快照时间戳（毫秒）
    pub ts_ms: i64,
}

/// 全局决策时间线条目。
#[derive(Serialize)]
pub struct DecisionFeedItem {
    pub account: String,
    pub ts_ms: i64,
    pub status: String,
    pub note: String,
}

/// 资产曲线点。
#[derive(Serialize)]
pub struct EquityPoint {
    pub ts: i64,
    pub total: f64,
}

/// 搜索标的（按代码/名称模糊匹配，limit 20）。
pub async fn search_symbols(
    State(st): State<WebState>,
    axum::extract::Query(params): axum::extract::Query<SearchParams>,
) -> Json<Vec<SymbolItem>> {
    let q = params.q.unwrap_or_default();
    let m = st.market.lock().unwrap();
    let mut out = Vec::new();
    if let Ok(rows) = m.search_tickers(&q, 20) {
        for (thscode, name, ticker) in rows {
            out.push(SymbolItem { thscode, name, ticker });
        }
    }
    Json(out)
}

#[derive(serde::Deserialize)]
pub struct SearchParams {
    pub q: Option<String>,
}

/// 日 K（近 120 根）+ 均线。
pub async fn kline(
    State(st): State<WebState>,
    Path(thscode): Path<String>,
) -> Result<Json<KlineResponse>, axum::http::StatusCode> {
    let m = st.market.lock().unwrap();
    let name = m.ticker_name(&thscode).unwrap_or_else(|_| thscode.clone());
    let bars = m.recent_bars(&thscode, 120).map_err(|_| axum::http::StatusCode::NOT_FOUND)?;
    let mut points = Vec::new();
    let mut closes: Vec<f64> = Vec::new();
    for b in &bars {
        closes.push(b.close);
        let ma = |n: usize| -> f64 {
            let len = closes.len();
            if len < n {
                return 0.0;
            }
            closes[len - n..].iter().sum::<f64>() / n as f64
        };
        points.push(KlinePoint {
            date: fmt_date(b.date_ms),
            ohlc: vec![b.open, b.close, b.low, b.high],
            volume: b.volume,
            ma5: ma(5),
            ma10: ma(10),
            ma20: ma(20),
            ma60: ma(60),
        });
    }
    // 盘中：用最新快照补今日实时走势（覆盖/追加最后一根）
    if let Some((price, open, high, low, vol)) = m.today_snapshot(&thscode).map_err(|_| axum::http::StatusCode::INTERNAL_SERVER_ERROR)? {
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        if let Some(last) = points.last_mut() {
            if last.date == today {
                // 同日：覆盖（最新价、今日高低、累计量）
                last.ohlc = vec![open, price, low, high];
                last.volume = vol;
            } else {
                // 新一天：追加（用快照数据）
                points.push(KlinePoint {
                    date: today,
                    ohlc: vec![open, price, low, high],
                    volume: vol,
                    ma5: 0.0, ma10: 0.0, ma20: 0.0, ma60: 0.0,
                });
            }
        }
    }
    Ok(Json(KlineResponse { thscode, name, points }))
}

/// 账户列表（含资产）。市值按行情库最新价计算（真实市值）。
pub async fn accounts(State(st): State<WebState>) -> Json<Vec<AccountAsset>> {
    let list = accounts::list_accounts(&st.cfg.data_dir).unwrap_or_default();
    let mut out = Vec::new();
    for a in &list {
        if let Ok(acct) = accounts::open_account(&st.cfg.data_dir, &a.name) {
            let (cash, positions, initial, sparkline, last_snap) = {
                let db = acct.lock().unwrap();
                let ac = match db.get_or_init_account(st.cfg.initial_capital) {
                    Ok(x) => x,
                    Err(_) => continue,
                };
                let sp: Vec<f64> = db.account_snapshots().unwrap_or_default()
                    .iter().map(|s| s.total_asset).collect();
                let last = sp.last().copied();
                (ac.cash, db.positions().unwrap_or_default(), ac.initial_capital, sp, last)
            };
            // 真实市值：持仓按行情库最新价
            let mv: f64 = {
                let m = st.market.lock().unwrap();
                positions.iter()
                    .map(|p| m.latest_snapshot_price(&p.thscode).ok().flatten().unwrap_or(p.avg_cost) * p.quantity as f64)
                    .sum()
            };
            let total = cash + mv;
            let rp = if initial > 0.0 { (total / initial - 1.0) * 100.0 } else { 0.0 };
            let today_pnl = last_snap.map(|prev| total - prev).unwrap_or(0.0);
            out.push(AccountAsset {
                name: a.name.clone(),
                cash,
                market_value: mv,
                total,
                return_pct: rp,
                position_count: positions.len(),
                today_pnl,
                sparkline: sparkline.iter().rev().take(20).rev().cloned().collect(),
            });
        }
    }
    Json(out)
}

/// 市场总览（全局状态条）：指数实时 + 涨跌家数 + 市场状态。
pub async fn market_overview(State(st): State<WebState>) -> Json<MarketOverview> {
    const INDEXES: &[(&str, &str)] = &[
        ("000001.SH", "上证指数"),
        ("399001.SZ", "深证成指"),
        ("399006.SZ", "创业板指"),
        ("000300.SH", "沪深300"),
        ("000905.SH", "中证500"),
    ];
    let m = st.market.lock().unwrap();
    let mut indexes = Vec::new();
    let mut latest_ts = 0i64;
    for (code, name) in INDEXES {
        let quote = m.latest_snapshot_full(code).ok().flatten();
        if let Some((price, _chg, pct, ts)) = quote {
            if ts > latest_ts { latest_ts = ts; }
            indexes.push(IndexQuote { thscode: code.to_string(), name: name.to_string(), price, pct });
        } else if let Ok(Some(b)) = m.recent_bars(code, 1).map(|v| v.into_iter().next()) {
            indexes.push(IndexQuote { thscode: code.to_string(), name: name.to_string(), price: b.close, pct: 0.0 });
        }
    }
    let (up, total) = m.market_breadth().unwrap_or((0, 0));
    let regime = if total == 0 { "neutral" } else {
        let r = up as f64 / total as f64;
        if r >= 0.6 { "risk-on" } else if r >= 0.4 { "neutral" } else { "risk-off" }
    }.to_string();
    Json(MarketOverview { indexes, up, total, regime, ts_ms: latest_ts })
}

/// 市场涨跌分布（分桶统计）。
pub async fn market_distribution(State(st): State<WebState>) -> Json<Vec<(String, u32)>> {
    let m = st.market.lock().unwrap();
    Json(m.market_distribution().unwrap_or_default())
}

/// 同花顺热股榜 TOP20（实时调上游 API；key 从库 meta 读，热生效）。
pub async fn market_hot(State(st): State<WebState>) -> Json<serde_json::Value> {
    let key = {
        let m = st.market.lock().unwrap();
        m.meta_get("hithink_api_key").ok().flatten().unwrap_or_default()
    };
    if key.is_empty() {
        return Json(serde_json::json!({ "item": [] }));
    }
    let client = match hithink_sdk::Client::new(key) {
        Ok(c) => c,
        Err(_) => return Json(serde_json::json!({ "item": [] })),
    };
    match client.hot_stock_list(Some("day")).await {
        Ok(v) => Json(v),
        Err(_) => Json(serde_json::json!({ "item": [] })),
    }
}

/// 全局决策时间线（合并所有账户，按时间倒序，limit 20）。
pub async fn recent_decisions(State(st): State<WebState>) -> Json<Vec<DecisionFeedItem>> {
    let list = accounts::list_accounts(&st.cfg.data_dir).unwrap_or_default();
    let mut all = Vec::new();
    for a in &list {
        if let Ok(acct) = accounts::open_account(&st.cfg.data_dir, &a.name) {
            let db = acct.lock().unwrap();
            for d in db.recent_decision_logs(10).unwrap_or_default() {
                all.push(DecisionFeedItem {
                    account: a.name.clone(),
                    ts_ms: d.ts_ms,
                    status: d.status.clone(),
                    note: d.note.clone(),
                });
            }
        }
    }
    all.sort_by(|a, b| b.ts_ms.cmp(&a.ts_ms));
    all.truncate(20);
    Json(all)
}

/// 删除账户（含其全部数据）。
pub async fn delete_account(
    State(st): State<WebState>,
    Path(name): Path<String>,
    headers: axum::http::HeaderMap,
) -> Result<Json<serde_json::Value>, axum::http::StatusCode> {
    if !crate::web::admin_ok(&st.cfg, &headers) {
        return Err(axum::http::StatusCode::UNAUTHORIZED);
    }
    accounts::remove_account(&st.cfg.data_dir, &name)
        .map_err(|_| axum::http::StatusCode::NOT_FOUND)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// 账户资产曲线。
pub async fn equity(
    State(st): State<WebState>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, axum::http::StatusCode> {
    let acct = accounts::open_account(&st.cfg.data_dir, &name).map_err(|_| axum::http::StatusCode::NOT_FOUND)?;
    let (snaps, initial) = {
        let db = acct.lock().unwrap();
        let snaps = db.account_snapshots().map_err(|_| axum::http::StatusCode::NOT_FOUND)?;
        let initial = db.get_or_init_account(st.cfg.initial_capital).map(|a| a.initial_capital).unwrap_or(0.0);
        (snaps, initial)
    };
    let points: Vec<EquityPoint> = snaps.iter().map(|s| EquityPoint { ts: s.ts_ms, total: s.total_asset }).collect();
    // 基准：沪深300 同区间净值化（首个快照日=初始资金）
    let mut benchmark: Vec<EquityPoint> = Vec::new();
    if let (Some(first), true) = (snaps.first(), initial > 0.0) {
        let m = st.market.lock().unwrap();
        if let Ok(bars) = m.recent_bars("000300.SH", 400) {
            let base = bars.iter().find(|b| b.date_ms <= first.ts_ms).map(|b| b.close)
                .or_else(|| bars.first().map(|b| b.close));
            if let Some(base) = base.filter(|b| *b > 0.0) {
                for b in &bars {
                    if b.date_ms >= first.ts_ms - 86_400_000 {
                        benchmark.push(EquityPoint { ts: b.date_ms, total: initial * b.close / base });
                    }
                }
            }
        }
    }
    Ok(Json(serde_json::json!({ "points": points, "benchmark": benchmark })))
}

/// 持仓风控状态行。
#[derive(Serialize)]
pub struct RiskRow {
    pub thscode: String,
    pub name: String,
    pub price: f64,
    pub avg_cost: f64,
    /// 距止损线（-5%）的百分比距离，如 3.2 表示现价离止损线还有 3.2%
    pub to_stop_pct: f64,
    /// 距止盈线（+15%）的百分比距离
    pub to_profit_pct: f64,
}

/// 账户风控状态：熔断 + 持仓止损/止盈距离 + 仓位。
pub async fn risk_status(
    State(st): State<WebState>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, axum::http::StatusCode> {
    let acct = accounts::open_account(&st.cfg.data_dir, &name).map_err(|_| axum::http::StatusCode::NOT_FOUND)?;
    let (positions, cash, fuse_until, peak) = {
        let db = acct.lock().unwrap();
        let ac = db.get_or_init_account(st.cfg.initial_capital).map_err(|_| axum::http::StatusCode::NOT_FOUND)?;
        let fuse = db.meta_get("fuse_until_ms").ok().flatten().and_then(|s| s.parse::<i64>().ok()).unwrap_or(0);
        let peak = db.meta_get("peak_asset").ok().flatten().and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0);
        (db.positions().unwrap_or_default(), ac.cash, fuse, peak)
    };
    let mut rows = Vec::new();
    let mut mv = 0.0;
    {
        let m = st.market.lock().unwrap();
        for p in &positions {
            let price = m.latest_snapshot_price(&p.thscode).ok().flatten().unwrap_or(p.avg_cost);
            mv += price * p.quantity as f64;
            let to_stop = if p.avg_cost > 0.0 { (price / (p.avg_cost * 0.95) - 1.0) * 100.0 } else { 0.0 };
            let to_profit = if p.avg_cost > 0.0 { (price / (p.avg_cost * 1.15) - 1.0) * 100.0 } else { 0.0 };
            rows.push(RiskRow { thscode: p.thscode.clone(), name: resolve_name(&m, &p.thscode, &p.name), price, avg_cost: p.avg_cost, to_stop_pct: to_stop, to_profit_pct: to_profit });
        }
    }
    let total = cash + mv;
    let now_ms = chrono::Utc::now().timestamp_millis();
    let drawdown = if peak > 0.0 { (peak - total) / peak * 100.0 } else { 0.0 };
    Ok(Json(serde_json::json!({
        "fuse_active": fuse_until > now_ms,
        "fuse_until_ms": fuse_until,
        "peak": peak,
        "total": total,
        "drawdown_pct": drawdown,
        "position_pct": if total > 0.0 { mv / total * 100.0 } else { 0.0 },
        "positions": rows,
    })))
}

/// 持仓行（含现价与浮动盈亏，价格读行情库）。
#[derive(Serialize)]
pub struct PositionRow {
    pub thscode: String,
    pub name: String,
    pub quantity: u32,
    pub avg_cost: f64,
    pub price: f64,
    pub pnl: f64,
    pub pnl_pct: f64,
}

/// 账户持仓（带最新价与浮动盈亏）。
pub async fn positions(
    State(st): State<WebState>,
    Path(name): Path<String>,
) -> Result<Json<Vec<PositionRow>>, axum::http::StatusCode> {
    let acct = accounts::open_account(&st.cfg.data_dir, &name).map_err(|_| axum::http::StatusCode::NOT_FOUND)?;
    let positions = acct.lock().unwrap().positions().map_err(|_| axum::http::StatusCode::NOT_FOUND)?;
    let mut out = Vec::new();
    {
        let m = st.market.lock().unwrap();
        for p in &positions {
            let price = m.latest_snapshot_price(&p.thscode).ok().flatten().unwrap_or(p.avg_cost);
            let pnl = (price - p.avg_cost) * p.quantity as f64;
            let pnl_pct = if p.avg_cost > 0.0 { (price / p.avg_cost - 1.0) * 100.0 } else { 0.0 };
            let pname = resolve_name(&m, &p.thscode, &p.name);
            out.push(PositionRow {
                thscode: p.thscode.clone(),
                name: pname,
                quantity: p.quantity,
                avg_cost: p.avg_cost,
                price,
                pnl,
                pnl_pct,
            });
        }
    }
    Ok(Json(out))
}

/// 成交行。
#[derive(Serialize)]
pub struct TradeRow {
    pub ts_ms: i64,
    pub thscode: String,
    pub name: String,
    pub side: String,
    pub quantity: u32,
    pub price: f64,
    pub amount: f64,
    pub fee: f64,
    /// 关联的 AI 决策 id（None = 风控触发）
    pub decision_id: Option<i64>,
}

/// 账户成交记录（最近 N 条）。
pub async fn trades(
    State(st): State<WebState>,
    Path(name): Path<String>,
) -> Result<Json<Vec<TradeRow>>, axum::http::StatusCode> {
    let acct = accounts::open_account(&st.cfg.data_dir, &name).map_err(|_| axum::http::StatusCode::NOT_FOUND)?;
    let rows = acct.lock().unwrap().recent_trades(50).map_err(|_| axum::http::StatusCode::NOT_FOUND)?;
    let m = st.market.lock().unwrap();
    Ok(Json(rows.into_iter().map(|t| {
        let tname = resolve_name(&m, &t.thscode, &t.name);
        TradeRow {
        ts_ms: t.ts_ms,
        thscode: t.thscode,
        name: tname,
        side: t.side.as_str().into(),
        quantity: t.quantity,
        price: t.price,
        amount: t.amount,
        fee: t.fee,
        decision_id: t.decision_id,
    }}).collect()))
}

/// 决策行。
#[derive(Serialize)]
pub struct DecisionRow {
    pub id: i64,
    pub ts_ms: i64,
    pub model: String,
    pub status: String,
    pub note: String,
    pub raw_response: String,
    /// 解析后的动作 JSON（buy/sell/hold 列表）
    pub actions: String,
    /// 复盘验证结果 [{days_after, pnl}]
    pub reviews: Vec<ReviewBrief>,
}

/// 复盘结果摘要。
#[derive(Serialize)]
pub struct ReviewBrief {
    pub days_after: u32,
    pub pnl: f64,
}

/// 账户 AI 决策记录。
pub async fn decisions(
    State(st): State<WebState>,
    Path(name): Path<String>,
) -> Result<Json<Vec<DecisionRow>>, axum::http::StatusCode> {
    let acct = accounts::open_account(&st.cfg.data_dir, &name).map_err(|_| axum::http::StatusCode::NOT_FOUND)?;
    let db = acct.lock().unwrap();
    let rows = db.recent_decision_logs(50).map_err(|_| axum::http::StatusCode::NOT_FOUND)?;
    Ok(Json(rows.into_iter().map(|d| {
        let reviews = db.reviews_for_decision(d.id).unwrap_or_default()
            .into_iter().map(|r| ReviewBrief { days_after: r.days_after, pnl: r.pnl }).collect();
        DecisionRow {
            id: d.id,
            ts_ms: d.ts_ms,
            model: d.model,
            status: d.status,
            note: d.note,
            raw_response: d.raw_response,
            actions: d.actions,
            reviews,
        }
    }).collect()))
}

/// 某条决策关联的成交记录。
pub async fn decision_trades(
    State(st): State<WebState>,
    Path((name, id)): Path<(String, i64)>,
) -> Result<Json<Vec<TradeRow>>, axum::http::StatusCode> {
    let acct = accounts::open_account(&st.cfg.data_dir, &name).map_err(|_| axum::http::StatusCode::NOT_FOUND)?;
    let db = acct.lock().unwrap();
    let rows = db.trades_for_decision(id).map_err(|_| axum::http::StatusCode::NOT_FOUND)?;
    Ok(Json(rows.into_iter().map(|t| TradeRow {
        ts_ms: t.ts_ms,
        thscode: t.thscode,
        name: t.name,
        side: t.side.as_str().into(),
        quantity: t.quantity,
        price: t.price,
        amount: t.amount,
        fee: t.fee,
        decision_id: t.decision_id,
    }).collect()))
}

/// 全部账户的收益曲线（归一化为收益率%，多账户对比用）。
pub async fn accounts_equity_all(State(st): State<WebState>) -> Json<Vec<serde_json::Value>> {
    let list = accounts::list_accounts(&st.cfg.data_dir).unwrap_or_default();
    let mut out = Vec::new();
    for a in &list {
        if let Ok(acct) = accounts::open_account(&st.cfg.data_dir, &a.name) {
            let db = acct.lock().unwrap();
            let snaps = db.account_snapshots().unwrap_or_default();
            let initial = db.get_or_init_account(st.cfg.initial_capital).map(|x| x.initial_capital).unwrap_or(0.0);
            if initial > 0.0 && !snaps.is_empty() {
                let pts: Vec<serde_json::Value> = snaps.iter().map(|s| serde_json::json!({
                    "ts": s.ts_ms,
                    "pct": (s.total_asset / initial - 1.0) * 100.0,
                })).collect();
                out.push(serde_json::json!({ "name": a.name, "points": pts }));
            }
        }
    }
    Json(out)
}

/// 名称补全：历史遗留记录 name 为空或等于代码时，从行情库查真实名称。
fn resolve_name(m: &finbox_store::Db, thscode: &str, name: &str) -> String {
    if name.is_empty() || name == thscode {
        m.ticker_name(thscode).unwrap_or_else(|_| thscode.to_string())
    } else {
        name.to_string()
    }
}

fn fmt_date(ms: i64) -> String {
    // date_ms 为 Asia/Shanghai 零点毫秒，格式化需加 8h 偏移（否则 UTC 会少一天）
    chrono::DateTime::from_timestamp_millis(ms + 8 * 3600 * 1000)
        .map(|t| t.format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}
