//! finbox Web 界面（axum）：账户列表 / 单账户 / 全局配置。

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{Html, IntoResponse, Redirect},
    routing::get,
    Form, Router,
};
use serde::Deserialize;

use crate::accounts;
use crate::config::Config;
use finbox_store::SharedDb;

pub struct WebState {
    pub cfg: Config,
    pub market: SharedDb,
}

impl Clone for WebState {
    fn clone(&self) -> Self {
        Self { cfg: self.cfg.clone(), market: self.market.clone() }
    }
}

impl WebState {
    pub fn new(cfg: &Config) -> anyhow::Result<Self> {
        let market = accounts::open_market(&cfg.data_dir)?;
        Ok(Self { cfg: cfg.clone(), market })
    }
}

pub fn router(state: WebState) -> Router {
    Router::new()
        .route("/", get(overview))
        .route("/account/{name}", get(account_page))
        .route("/accounts/new", get(new_account_page).post(create_account))
        .route("/config", get(config_page).post(save_config))
        .with_state(state)
}

fn layout(title: &str, body: &str) -> Html<String> {
    Html(format!(
        r#"<!doctype html><html lang="zh"><head><meta charset="utf-8">
<title>{title} · finbox</title>
<style>
body{{font-family:-apple-system,sans-serif;margin:24px;max-width:1000px;margin-inline:auto;color:#222}}
a{{margin-right:12px}}table{{border-collapse:collapse;width:100%}}
th,td{{border:1px solid #ddd;padding:6px 10px;font-size:14px;text-align:left}}
th{{background:#f5f5f5}}h1{{font-size:22px}}h2{{font-size:17px}}.num{{font-variant-numeric:tabular-nums}}
input{{padding:4px 6px;margin:2px}}
</style></head><body>
<h1>finbox · A股模拟交易（单进程多账户）</h1>
<nav><a href="/">账户</a><a href="/accounts/new">新建账户</a><a href="/config">配置</a></nav>
<hr>{body}</body></html>"#
    ))
}

async fn overview(State(st): State<WebState>) -> impl IntoResponse {
    let list = accounts::list_accounts(&st.cfg.data_dir).unwrap_or_default();
    let mut rows = Vec::new();
    for a in &list {
        if let Ok(acct) = accounts::open_account(&st.cfg.data_dir, &a.name) {
            let db = acct.lock().unwrap();
            let ac = db.get_or_init_account(st.cfg.initial_capital).unwrap_or(finbox_core::Account { cash: 0.0, initial_capital: 0.0 });
            let total = db.total_asset_estimate(&ac).unwrap_or(ac.cash);
            let pnl = if ac.initial_capital > 0.0 { (total / ac.initial_capital - 1.0) * 100.0 } else { 0.0 };
            let npos = db.positions().unwrap_or_default().len();
            rows.push(format!(
                "<tr><td><a href='/account/{}'>{}</a></td><td class=num>{:.2}</td><td class=num>{:.2}</td><td class=num>{:+.2}%</td><td class=num>{}</td></tr>",
                a.name, a.name, ac.cash, total, pnl, npos
            ));
        }
    }
    let body = format!(
        "<h2>账户列表（{} 个）</h2>
        <table><tr><th>账户</th><th>现金</th><th>总资产</th><th>收益率</th><th>持仓数</th></tr>{}</table>
        <p><a href='/accounts/new'>+ 新建账户</a></p>",
        rows.len(),
        rows.join("")
    );
    layout("账户", &body)
}

async fn new_account_page() -> impl IntoResponse {
    let body = r#"<h2>新建账户</h2>
    <form method=post>
    <table>
    <tr><td>账户名</td><td><input name=name placeholder="如：稳健A"></td></tr>
    <tr><td>初始资金</td><td><input name=capital type=number value=200000></td></tr>
    <tr><td>自选池(逗号分隔thscode)</td><td><input name=watchlist style=width:300px></td></tr>
    </table><p><button type=submit>创建</button></p></form>"#;
    layout("新建账户", body)
}

#[derive(Deserialize)]
struct NewAccountForm {
    name: String,
    capital: f64,
    watchlist: String,
}

async fn create_account(
    State(st): State<WebState>,
    Form(form): Form<NewAccountForm>,
) -> Result<Redirect, StatusCode> {
    let info = accounts::create_account(&st.cfg.data_dir, &form.name, form.capital)
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    if !form.watchlist.is_empty() {
        let db = accounts::open_account(&st.cfg.data_dir, &info.name).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        db.lock().unwrap().meta_set("watchlist", &form.watchlist).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    }
    Ok(Redirect::to(&format!("/account/{}", info.name)))
}

async fn account_page(State(st): State<WebState>, Path(name): Path<String>) -> impl IntoResponse {
    let Ok(acct) = accounts::open_account(&st.cfg.data_dir, &name) else {
        return layout("账户不存在", &format!("<p>账户「{name}」不存在</p>"));
    };
    let db = acct.lock().unwrap();
    let ac = db.get_or_init_account(st.cfg.initial_capital).unwrap_or(finbox_core::Account { cash: 0.0, initial_capital: 0.0 });
    let total = db.total_asset_estimate(&ac).unwrap_or(ac.cash);
    let positions = db.positions().unwrap_or_default();
    let trades = db.recent_trades(20).unwrap_or_default();
    let decisions = db.recent_decision_logs(10).unwrap_or_default();
    let snapshots = db.account_snapshots().unwrap_or_default();

    let body = format!(
        "<h2>账户「{name}」</h2>
        <table><tr><th>现金</th><th>市值(估算)</th><th>总资产</th><th>收益率</th></tr>
        <tr><td class=num>{:.2}</td><td class=num>{:.2}</td><td class=num>{:.2}</td><td class=num>{:+.2}%</td></tr></table>
        <h2>资产曲线</h2>
        <table><tr><th>时间</th><th>总资产</th></tr>{}</table>
        <h2>持仓（{} 只）</h2>
        <table><tr><th>代码</th><th>名称</th><th>数量</th><th>成本</th></tr>{}</table>
        <h2>最近流水</h2>
        <table><tr><th>时间</th><th>方向</th><th>代码</th><th>数量</th><th>价格</th><th>费用</th></tr>{}</table>
        <h2>最近决策</h2>
        <table><tr><th>时间</th><th>状态</th><th>备注</th></tr>{}</table>",
        ac.cash, total - ac.cash, total,
        if ac.initial_capital > 0.0 { (total / ac.initial_capital - 1.0) * 100.0 } else { 0.0 },
        snapshots.iter().rev().take(10).map(|s| format!(
            "<tr><td>{}</td><td class=num>{:.2}</td></tr>", fmt_ms(s.ts_ms), s.total_asset
        )).collect::<Vec<_>>().join(""),
        positions.len(),
        positions.iter().map(|p| format!(
            "<tr><td>{}</td><td>{}</td><td class=num>{}</td><td class=num>{:.3}</td></tr>",
            p.thscode, p.name, p.quantity, p.avg_cost
        )).collect::<Vec<_>>().join(""),
        trades.iter().map(|t| format!(
            "<tr><td>{}</td><td>{}</td><td>{}</td><td class=num>{}</td><td class=num>{:.2}</td><td class=num>{:.2}</td></tr>",
            fmt_ms(t.ts_ms), t.side.as_str(), t.thscode, t.quantity, t.price, t.fee
        )).collect::<Vec<_>>().join(""),
        decisions.iter().map(|d| format!(
            "<tr><td>{}</td><td>{}</td><td>{}</td></tr>", fmt_ms(d.ts_ms), d.status, esc(&d.note)
        )).collect::<Vec<_>>().join(""),
    );
    layout(&format!("{name} · 账户"), &body)
}

#[derive(Deserialize)]
struct ConfigForm {
    hithink_api_key: String,
    llm_api_key: String,
    llm_base_url: String,
    llm_model: String,
    candidate_count: usize,
    collect_interval_seconds: u64,
}

async fn config_page(State(st): State<WebState>) -> impl IntoResponse {
    let c = &st.cfg;
    let (hk, lk) = {
        let m = st.market.lock().unwrap();
        let h = m.meta_get("hithink_api_key").ok().flatten().unwrap_or_else(|| c.hithink_api_key.clone());
        let l = m.meta_get("llm_api_key").ok().flatten().unwrap_or_else(|| c.llm_api_key.clone());
        (h, l)
    };
    let body = format!(
        r#"<h2>全局配置（保存到行情库 meta，运行中的进程即时生效）</h2>
        <form method=post>
        <table>
        <tr><td>同花顺 API Key</td><td><input name=hithink_api_key value="{}" style=width:360px></td></tr>
        <tr><td>LLM API Key</td><td><input name=llm_api_key value="{}" style=width:360px></td></tr>
        <tr><td>LLM Base URL</td><td><input name=llm_base_url value="{}" style=width:360px></td></tr>
        <tr><td>LLM 模型</td><td><input name=llm_model value="{}" style=width:200px></td></tr>
        <tr><td>候选数(少而精)</td><td><input name=candidate_count value="{}" type=number></td></tr>
        <tr><td>采集间隔(秒)</td><td><input name=collect_interval_seconds value="{}" type=number></td></tr>
        </table><p><button type=submit>保存</button></p></form>
        <p><small>注意：运行中的采集/决策会在每次调用时重新读取 key，无需重启。</small></p>"#,
        esc(&hk), esc(&lk), esc(&c.llm_base_url), esc(&c.llm_model), c.candidate_count, c.collect_interval_seconds
    );
    layout("配置", &body)
}

async fn save_config(
    State(st): State<WebState>,
    Form(form): Form<ConfigForm>,
) -> Result<Redirect, StatusCode> {
    let m = st.market.lock().unwrap();
    m.meta_set("hithink_api_key", &form.hithink_api_key).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    m.meta_set("llm_api_key", &form.llm_api_key).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    m.meta_set("llm_base_url", &form.llm_base_url).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    m.meta_set("llm_model", &form.llm_model).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    m.meta_set("candidate_count", &form.candidate_count.to_string()).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    m.meta_set("collect_interval_seconds", &form.collect_interval_seconds.to_string()).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Redirect::to("/config"))
}

fn fmt_ms(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "-".into())
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}
