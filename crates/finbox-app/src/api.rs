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
    Ok(Json(KlineResponse { thscode, name, points }))
}

/// 账户列表（含资产）。
pub async fn accounts(State(st): State<WebState>) -> Json<Vec<AccountAsset>> {
    let list = accounts::list_accounts(&st.cfg.data_dir).unwrap_or_default();
    let mut out = Vec::new();
    for a in &list {
        if let Ok(acct) = accounts::open_account(&st.cfg.data_dir, &a.name) {
            let db = acct.lock().unwrap();
            if let Ok(ac) = db.get_or_init_account(st.cfg.initial_capital) {
                let total = db.total_asset_estimate(&ac).unwrap_or(ac.cash);
                let rp = if ac.initial_capital > 0.0 { (total / ac.initial_capital - 1.0) * 100.0 } else { 0.0 };
                let npos = db.positions().unwrap_or_default().len();
                out.push(AccountAsset {
                    name: a.name.clone(),
                    cash: ac.cash,
                    market_value: total - ac.cash,
                    total,
                    return_pct: rp,
                    position_count: npos,
                });
            }
        }
    }
    Json(out)
}

/// 删除账户（含其全部数据）。
pub async fn delete_account(
    State(st): State<WebState>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, axum::http::StatusCode> {
    accounts::remove_account(&st.cfg.data_dir, &name)
        .map_err(|_| axum::http::StatusCode::NOT_FOUND)?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// 账户资产曲线。
pub async fn equity(
    State(st): State<WebState>,
    Path(name): Path<String>,
) -> Result<Json<Vec<EquityPoint>>, axum::http::StatusCode> {
    let acct = accounts::open_account(&st.cfg.data_dir, &name).map_err(|_| axum::http::StatusCode::NOT_FOUND)?;
    let db = acct.lock().unwrap();
    let snaps = db.account_snapshots().map_err(|_| axum::http::StatusCode::NOT_FOUND)?;
    Ok(Json(snaps.into_iter().map(|s| EquityPoint { ts: s.ts_ms, total: s.total_asset }).collect()))
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
            out.push(PositionRow {
                thscode: p.thscode.clone(),
                name: p.name.clone(),
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
}

/// 账户成交记录（最近 N 条）。
pub async fn trades(
    State(st): State<WebState>,
    Path(name): Path<String>,
) -> Result<Json<Vec<TradeRow>>, axum::http::StatusCode> {
    let acct = accounts::open_account(&st.cfg.data_dir, &name).map_err(|_| axum::http::StatusCode::NOT_FOUND)?;
    let db = acct.lock().unwrap();
    let rows = db.recent_trades(50).map_err(|_| axum::http::StatusCode::NOT_FOUND)?;
    Ok(Json(rows.into_iter().map(|t| TradeRow {
        ts_ms: t.ts_ms,
        thscode: t.thscode,
        name: t.name,
        side: t.side.as_str().into(),
        quantity: t.quantity,
        price: t.price,
        amount: t.amount,
        fee: t.fee,
    }).collect()))
}

/// 决策行。
#[derive(Serialize)]
pub struct DecisionRow {
    pub ts_ms: i64,
    pub model: String,
    pub status: String,
    pub note: String,
    pub raw_response: String,
}

/// 账户 AI 决策记录。
pub async fn decisions(
    State(st): State<WebState>,
    Path(name): Path<String>,
) -> Result<Json<Vec<DecisionRow>>, axum::http::StatusCode> {
    let acct = accounts::open_account(&st.cfg.data_dir, &name).map_err(|_| axum::http::StatusCode::NOT_FOUND)?;
    let db = acct.lock().unwrap();
    let rows = db.recent_decision_logs(50).map_err(|_| axum::http::StatusCode::NOT_FOUND)?;
    Ok(Json(rows.into_iter().map(|d| DecisionRow {
        ts_ms: d.ts_ms,
        model: d.model,
        status: d.status,
        note: d.note,
        raw_response: d.raw_response,
    }).collect()))
}

fn fmt_date(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|t| t.format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}
