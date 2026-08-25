//! finbox Web 界面（深色金融风）。
//!
//! 页面：概览 / 行情(K线) / 持仓 / 交易 / AI建议 / 设置
//! 顶部导航 + 账户切换器；数据经 JSON API 加载，图表用 ECharts（本地静态）。

use axum::{
    body::Body,
    extract::State,
    http::{header, StatusCode},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
    Form, Router,
};
use rust_embed::RustEmbed;
use serde::Deserialize;

use crate::accounts;
use crate::config::Config;
use crate::{api};

/// 静态资源（编译期嵌入，单二进制自包含）。
#[derive(RustEmbed)]
#[folder = "static/"]
struct Static;

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

/// 启动 Web 服务（供 `run` 同进程调用或独立启动）。
pub async fn serve(cfg: &Config, bind: &str) -> anyhow::Result<()> {
    let state = WebState::new(cfg)?;
    let app = router(state);
    let listener = tokio::net::TcpListener::bind(bind).await?;
    log::info!("Web 界面已启动: http://{bind}");
    axum::serve(listener, app).await?;
    Ok(())
}

pub fn router(state: WebState) -> Router {
    Router::new()
        .route("/", get(home_page))
        .route("/market", get(market_page))
        .route("/account/{name}", get(account_page))
        .route("/account/{name}/edit", get(account_settings_page))
        .route("/api/account/{name}/settings", post(account_settings_save))
        .route("/settings", get(settings_page).post(save_settings))
        .route("/accounts/new", get(new_account_page).post(create_account))
        // JSON API
        .route("/api/search", get(api::search_symbols))
        .route("/api/kline/{code}", get(api::kline))
        .route("/api/accounts", get(api::accounts))
        .route("/api/market/overview", get(api::market_overview))
        .route("/api/market/distribution", get(api::market_distribution))
        .route("/api/market/hot", get(api::market_hot))
        .route("/api/decisions/recent", get(api::recent_decisions))
        .route("/api/account/{name}/equity", get(api::equity))
        .route("/api/account/{name}/positions", get(api::positions))
        .route("/api/account/{name}/trades", get(api::trades))
        .route("/api/account/{name}/decisions", get(api::decisions))
        .route("/api/account/{name}", axum::routing::delete(api::delete_account))
        // 静态资源（编译期嵌入）
        .route("/static/{file}", get(static_file))
        .with_state(state)
}

/// 从嵌入资源提供静态文件（style.css / app.js / echarts.min.js）。
async fn static_file(axum::extract::Path(file): axum::extract::Path<String>) -> Response {
    match Static::get(&file) {
        Some(content) => {
            let mime = match file.rsplit('.').next().unwrap_or("") {
                "css" => "text/css; charset=utf-8",
                "js" => "application/javascript; charset=utf-8",
                "html" => "text/html; charset=utf-8",
                _ => "application/octet-stream",
            };
            (
                [(header::CONTENT_TYPE, mime)],
                Body::from(content.data.into_owned()),
            )
                .into_response()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
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
  <div class="brand">finbox</div>
  <nav class="mainnav">{navo}{navm}{navs}</nav>
  <div class="sys-status" id="sys-status"></div>
</header>
<div class="statusbar" id="statusbar"></div>
<main class="content">{body}</main>
<footer class="foot">finbox</footer>
<script src="/static/echarts.min.js"></script>
<script>window.ACTIVE_ACCT = localStorage.getItem('finbox_acct') || '';</script>
<script src="/static/app.js"></script>
</body></html>"#,
        navo = nav("", "模拟"),
        navm = nav("market", "行情"),
        navs = nav("settings", "设置"),
    ))
}

// ---- 模拟主页：账户列表（驾驶舱） ----
async fn home_page() -> impl IntoResponse {
    let body = r#"<div class="overview-cards" id="overview-cards"></div>
<div class="panel">
  <div class="panel-head"><h2>模拟账户</h2><a class="btn-new" href="/accounts/new">+ 新建账户</a></div>
  <div class="acct-grid" id="acct-grid"></div>
  <div class="empty" id="acct-empty" style="display:none">还没有账户，点击右上角「+ 新建账户」开始</div>
</div>
<div class="panel">
  <h2>今日决策动态</h2>
  <div id="decision-feed" class="feed"></div>
</div>"#;
    layout("模拟", "", body)
}

// ---- 账户详情页 ----
async fn account_page(State(st): State<WebState>, axum::extract::Path(name): axum::extract::Path<String>) -> impl IntoResponse {
    // 校验账户存在
    if accounts::open_account(&st.cfg.data_dir, &name).is_err() {
        return layout("账户不存在", "", &format!("<p class='empty'>账户「{}」不存在，<a href='/'>返回模拟</a></p>", esc(&name)));
    }
    let name_esc = esc(&name);
    let body = format!(r#"<p><a href="/" class="back">← 返回模拟</a></p>
<div class="panel-head"><h2 id="acct-title">账户「{name}」</h2>
  <a class="btn-ghost" href="/account/{name}/edit">参数设置</a></div>
<div class="cards" id="acct-cards"></div>
<div class="panel"><h2>资产曲线</h2><div id="equity-chart" class="chart"></div></div>
<div class="tabs" id="acct-tabs">
  <button class="tab active" data-tab="positions">持仓</button>
  <button class="tab" data-tab="trades">成交</button>
  <button class="tab" data-tab="decisions">AI 建议</button>
</div>
<div class="tab-pane" id="tab-positions"></div>
<div class="tab-pane" id="tab-trades" style="display:none"></div>
<div class="tab-pane" id="tab-decisions" style="display:none"></div>"#, name = name_esc);
    layout(&format!("{name} · 账户"), "account", &body)
}

// ---- 账户参数页 ----
async fn account_settings_page(State(st): State<WebState>, axum::extract::Path(name): axum::extract::Path<String>) -> impl IntoResponse {
    let Ok(acct) = accounts::open_account(&st.cfg.data_dir, &name) else {
        return layout("账户不存在", "", &format!("<p class='empty'>账户「{name}」不存在，<a href='/'>返回模拟</a></p>"));
    };
    let db = acct.lock().unwrap();
    let get = |k: &str, def: &str| db.meta_get(k).ok().flatten().unwrap_or_else(|| def.to_string());
    let capital = db.get_or_init_account(0.0).map(|a| a.initial_capital.to_string()).unwrap_or_default();
    let body = format!(r#"<p><a href="/account/{name}" class="back">← 返回账户</a></p>
<section class="panel"><h2>账户参数 · {name}</h2>
<form method=post action="/api/account/{name}/settings" class="form">
  <label>初始资金(元) <input name=initial_capital value="{capital}" type=number></label>
  <label>自选池(逗号分隔) <input name=watchlist value="{wl}"></label>
  <label>决策间隔(分钟) <input name=decision_interval_minutes value="{di}" type=number></label>
  <label>候选股数量 <input name=candidate_count value="{cc}" type=number></label>
  <button type=submit>保存</button>
</form></section>"#,
        name = esc(&name), capital = esc(&capital), wl = esc(&get("watchlist", "")),
        di = esc(&get("decision_interval_minutes", "30")), cc = esc(&get("candidate_count", "5")));
    layout(&format!("{name} · 参数"), "account", &body)
}

#[derive(Deserialize)]
struct AccountSettingsForm {
    initial_capital: f64,
    watchlist: String,
    decision_interval_minutes: u64,
    candidate_count: usize,
}

/// 保存账户参数（写账户库 meta，热生效）。
async fn account_settings_save(
    State(st): State<WebState>,
    axum::extract::Path(name): axum::extract::Path<String>,
    Form(form): Form<AccountSettingsForm>,
) -> Result<Redirect, StatusCode> {
    let acct = accounts::open_account(&st.cfg.data_dir, &name).map_err(|_| StatusCode::NOT_FOUND)?;
    let db = acct.lock().unwrap();
    db.meta_set("initial_capital", &form.initial_capital.to_string()).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    db.meta_set("watchlist", &form.watchlist).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    db.meta_set("decision_interval_minutes", &form.decision_interval_minutes.to_string()).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    db.meta_set("candidate_count", &form.candidate_count.to_string()).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    // 同步账户表初始资金（仅当账户现金未动过时）
    let _ = db.get_or_init_account(form.initial_capital);
    Ok(Redirect::to(&format!("/account/{name}")))
}


// ---- 行情页（K线 + 市场全景）----
async fn market_page() -> impl IntoResponse {    let body = r#"<div class="panel">
  <div class="index-tabs" id="index-tabs"></div>
  <div class="searchbar" style="margin-top:12px">
    <input id="sym-search" placeholder="搜索个股：输入代码或名称，如 600519 / 贵州茅台" autocomplete="off">
    <div id="sym-suggest" class="suggest"></div>
  </div>
</div>
<div class="market-grid">
  <section class="panel">
    <div class="kline-head">
      <h2 id="kline-name">加载中…</h2>
      <span id="kline-quote" class="quote"></span>
    </div>
    <div id="kline-chart" class="chart chart-lg"></div>
  </section>
  <aside class="market-side">
    <div class="panel">
      <h2>涨跌分布</h2>
      <div id="dist-chart" class="chart" style="height:180px"></div>
    </div>
    <div class="panel">
      <h2>热股榜 TOP10</h2>
      <div id="hot-list" class="hot-list"><div class="empty">加载中…</div></div>
    </div>
  </aside>
</div>"#;
    layout("行情", "market", body)
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
