//! finbox Web 界面（深色金融风）。
//!
//! 页面：概览 / 行情(K线) / 持仓 / 交易 / AI建议 / 设置
//! 顶部导航 + 账户切换器；数据经 JSON API 加载，图表用 ECharts（本地静态）。

use axum::{
    extract::State,
    http::StatusCode,
    response::{Html, IntoResponse, Redirect},
    routing::get,
    Form, Router,
};
use serde::Deserialize;
use tower_http::services::ServeDir;

use crate::accounts;
use crate::config::Config;
use crate::{api};

pub struct WebState {
    pub cfg: Config,
    pub market: finbox_store::SharedDb,
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
        .route("/market", get(market_page))
        .route("/positions", get(positions_page))
        .route("/trades", get(trades_page))
        .route("/decisions", get(decisions_page))
        .route("/settings", get(settings_page).post(save_settings))
        .route("/accounts/new", get(new_account_page).post(create_account))
        // JSON API
        .route("/api/search", get(api::search_symbols))
        .route("/api/kline/{code}", get(api::kline))
        .route("/api/accounts", get(api::accounts))
        .route("/api/account/{name}/equity", get(api::equity))
        .route("/api/account/{name}/positions", get(api::positions))
        .route("/api/account/{name}/trades", get(api::trades))
        .route("/api/account/{name}/decisions", get(api::decisions))
        // 静态资源（echarts 等）
        .nest_service("/static", ServeDir::new("crates/finbox-app/static"))
        .with_state(state)
}

/// 顶部导航 + 账户切换器。
fn layout(title: &str, active: &str, body: &str) -> Html<String> {
    let nav = |id: &str, label: &str| {
        let cls = if id == active { "active" } else { "" };
        format!("<a class='{cls}' href='/{id}'>{label}</a>")
    };
    Html(format!(
        r#"<!doctype html><html lang="zh"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title} · finbox</title>
<link rel="stylesheet" href="/static/style.css">
</head><body>
<header class="topbar">
  <div class="brand">finbox <span class="brand-sub">AI 模拟交易</span></div>
  <nav class="mainnav">{navo}{navm}{navp}{navt}{navd}{navs}
    <a class="btn-new" href="/accounts/new">+ 新建账户</a>
  </nav>
  <div class="acct-switch">
    <select id="acct-select"><option>选择账户…</option></select>
  </div>
</header>
<main class="content">{body}</main>
<footer class="foot">finbox · 模拟盘数据仅供学习，非投资建议</footer>
<script src="/static/echarts.min.js"></script>
<script>window.ACTIVE_ACCT = localStorage.getItem('finbox_acct') || '';</script>
<script src="/static/app.js"></script>
</body></html>"#,
        navo = nav("", "概览"),
        navm = nav("market", "行情"),
        navp = nav("positions", "持仓"),
        navt = nav("trades", "交易"),
        navd = nav("decisions", "AI 建议"),
        navs = nav("settings", "设置"),
    ))
}

// ---- 概览 ----
async fn overview(State(_st): State<WebState>) -> impl IntoResponse {
    let body = r#"
<div class="cards" id="ov-cards"></div>
<div class="grid-2">
  <section class="panel">
    <h2>资产曲线</h2>
    <div id="equity-chart" class="chart"></div>
  </section>
  <section class="panel">
    <h2>当前持仓</h2>
    <table class="tbl" id="ov-positions"><thead><tr>
      <th>代码</th><th>名称</th><th>数量</th><th>成本</th><th>现价</th><th>浮动盈亏</th></tr></thead>
      <tbody></tbody></table>
    <div class="empty" id="ov-pos-empty">（空仓）</div>
  </section>
</div>
<div class="grid-2">
  <section class="panel"><h2>最近成交</h2>
    <table class="tbl" id="ov-trades"><thead><tr>
      <th>时间</th><th>方向</th><th>代码</th><th>数量</th><th>价格</th></tr></thead>
      <tbody></tbody></table></section>
  <section class="panel"><h2>最近 AI 建议</h2>
    <div id="ov-decisions"></div></section>
</div>"#;
    layout("概览", "overview", body)
}

// ---- 行情页（K线）----
async fn market_page() -> impl IntoResponse {
    let body = r#"<div class="panel">
  <div class="index-tabs" id="index-tabs"></div>
  <div class="searchbar" style="margin-top:12px">
    <input id="sym-search" placeholder="搜索个股：输入代码或名称，如 600519 / 贵州茅台" autocomplete="off">
    <div id="sym-suggest" class="suggest"></div>
  </div>
</div>
<section class="panel">
  <div class="kline-head">
    <h2 id="kline-name">加载中…</h2>
    <span id="kline-quote" class="quote"></span>
  </div>
  <div id="kline-chart" class="chart chart-lg"></div>
</section>"#;
    layout("行情", "market", body)
}

// ---- 持仓 ----
async fn positions_page() -> impl IntoResponse {
    let body = r#"<section class="panel"><h2>持仓</h2>
    <table class="tbl" id="pos-table"><thead><tr>
      <th>代码</th><th>名称</th><th>数量</th><th>成本</th><th>现价</th><th>浮动盈亏</th><th>盈亏率</th></tr></thead>
      <tbody></tbody></table>
    <div class="empty" id="pos-empty">（空仓）</div></section>"#;
    layout("持仓", "positions", body)
}

// ---- 交易 ----
async fn trades_page() -> impl IntoResponse {
    let body = r#"<section class="panel"><h2>成交流水</h2>
    <table class="tbl" id="trades-table"><thead><tr>
      <th>时间</th><th>方向</th><th>代码</th><th>名称</th><th>数量</th><th>价格</th><th>金额</th><th>费用</th></tr></thead>
      <tbody></tbody></table></section>"#;
    layout("交易", "trades", body)
}

// ---- AI 建议 ----
async fn decisions_page() -> impl IntoResponse {
    let body = r#"<section class="panel"><h2>AI 决策记录</h2>
    <div id="dec-list"></div></section>"#;
    layout("AI 建议", "decisions", body)
}

// ---- 设置 ----
async fn settings_page(State(st): State<WebState>) -> impl IntoResponse {
    let c = &st.cfg;
    let (hk, lk) = {
        let m = st.market.lock().unwrap();
        let h = m.meta_get("hithink_api_key").ok().flatten().unwrap_or_else(|| c.hithink_api_key.clone());
        let l = m.meta_get("llm_api_key").ok().flatten().unwrap_or_else(|| c.llm_api_key.clone());
        (h, l)
    };
    let body = format!(
        r#"<section class="panel"><h2>设置</h2>
        <p class="hint">配置保存后即时生效，无需重启。</p>
        <form method=post class="form">
          <label>同花顺数据 Key <input name=hithink_api_key value="{}" type=text></label>
          <label>AI Key <input name=llm_api_key value="{}" type=text></label>
          <label>AI 服务地址 <input name=llm_base_url value="{}" type=text></label>
          <label>AI 模型 <input name=llm_model value="{}" type=text></label>
          <label>候选股数量 <input name=candidate_count value="{}" type=number></label>
          <label>行情刷新间隔(秒) <input name=collect_interval_seconds value="{}" type=number></label>
          <button type=submit>保存</button>
        </form></section>"#,
        esc(&hk), esc(&lk), esc(&c.llm_base_url), esc(&c.llm_model), c.candidate_count, c.collect_interval_seconds
    );
    layout("设置", "settings", &body)
}

#[derive(Deserialize)]
struct SettingsForm {
    hithink_api_key: String,
    llm_api_key: String,
    llm_base_url: String,
    llm_model: String,
    candidate_count: usize,
    collect_interval_seconds: u64,
}

async fn save_settings(
    State(st): State<WebState>,
    Form(form): Form<SettingsForm>,
) -> Result<Redirect, StatusCode> {
    let m = st.market.lock().unwrap();
    m.meta_set("hithink_api_key", &form.hithink_api_key).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    m.meta_set("llm_api_key", &form.llm_api_key).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    m.meta_set("llm_base_url", &form.llm_base_url).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    m.meta_set("llm_model", &form.llm_model).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    m.meta_set("candidate_count", &form.candidate_count.to_string()).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    m.meta_set("collect_interval_seconds", &form.collect_interval_seconds.to_string()).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Redirect::to("/settings"))
}

// ---- 新建账户 ----
async fn new_account_page() -> impl IntoResponse {
    let body = r#"<section class="panel"><h2>新建模拟账户</h2>
    <form method=post class="form">
      <label>账户名称 <input name=name placeholder="如：稳健型"></label>
      <label>初始资金(元) <input name=capital type=number value=200000></label>
      <label>自选池(逗号分隔代码) <input name=watchlist></label>
      <button type=submit>创建</button>
    </form></section>"#;
    layout("新建账户", "", body)
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
    Ok(Redirect::to("/"))
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}
