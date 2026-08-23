//! finbox Web 界面（axum）：概览 / 持仓 / 流水 / 决策日志 / 配置。

use axum::{
    extract::State,
    http::StatusCode,
    response::{Html, IntoResponse, Redirect},
    routing::{get},
    Form, Router,
};
use serde::Deserialize;

use crate::config::Config;
use finbox_store::SharedDb;

pub struct WebState {
    pub db: SharedDb,
    pub cfg: Config,
}

impl Clone for WebState {
    fn clone(&self) -> Self {
        Self { db: self.db.clone(), cfg: self.cfg.clone() }
    }
}

/// 构建路由。
pub fn router(state: WebState) -> Router {
    Router::new()
        .route("/", get(overview))
        .route("/positions", get(positions_page))
        .route("/trades", get(trades_page))
        .route("/decisions", get(decisions_page))
        .route("/config", get(config_page).post(save_config))
        .with_state(state)
}

fn layout(title: &str, body: &str) -> Html<String> {
    Html(format!(
        r#"<!doctype html><html lang="zh"><head><meta charset="utf-8">
<title>{title} · finbox</title>
<style>
body{{font-family:-apple-system,sans-serif;margin:24px;max-width:960px;margin-inline:auto;color:#222}}
a{{margin-right:12px}}table{{border-collapse:collapse;width:100%}}
th,td{{border:1px solid #ddd;padding:6px 10px;font-size:14px;text-align:left}}
th{{background:#f5f5f5}}h1{{font-size:22px}}.num{{font-variant-numeric:tabular-nums}}
.pos{{color:#c0392b}}.neg{{color:#27ae60}}
</style></head><body>
<h1>finbox · A股模拟交易</h1>
<nav><a href="/">概览</a><a href="/positions">持仓</a><a href="/trades">流水</a>
<a href="/decisions">决策日志</a><a href="/config">配置</a></nav>
<hr>{body}</body></html>"#
    ))
}

async fn overview(State(st): State<WebState>) -> impl IntoResponse {
    let db = st.db.lock().unwrap();
    let acct = db.get_or_init_account(st.cfg.initial_capital).unwrap_or(finbox_core::Account { cash: 0.0, initial_capital: 0.0 });
    let total = db.total_asset(&acct).unwrap_or(acct.cash);
    let snapshots = db.account_snapshots().unwrap_or_default();
    let positions = db.positions().unwrap_or_default();
    let decisions = db.recent_decision_logs(10).unwrap_or_default();

    let body = format!(
        "<h2>账户概览</h2>
        <table><tr><th>现金</th><th>持仓市值</th><th>总资产</th><th>收益率</th></tr>
        <tr><td class=num>{:.2}</td><td class=num>{:.2}</td><td class=num>{:.2}</td>
        <td class=num>{:+.2}%</td></tr></table>
        <h2>资产曲线（最近 {} 个收盘）</h2>
        <table><tr><th>时间</th><th>现金</th><th>市值</th><th>总资产</th></tr>{}</table>
        <h2>当前持仓（{} 只）</h2>
        <table><tr><th>代码</th><th>名称</th><th>数量</th><th>成本</th></tr>{}</table>
        <h2>最近决策</h2>
        <table><tr><th>时间</th><th>状态</th><th>备注</th></tr>{}</table>",
        acct.cash, total - acct.cash, total,
        if acct.initial_capital > 0.0 { (total / acct.initial_capital - 1.0) * 100.0 } else { 0.0 },
        snapshots.len(),
        snapshots.iter().rev().take(10).map(|s| format!(
            "<tr><td>{}</td><td class=num>{:.2}</td><td class=num>{:.2}</td><td class=num>{:.2}</td></tr>",
            fmt_ms(s.ts_ms), s.cash, s.market_value, s.total_asset
        )).collect::<Vec<_>>().join(""),
        positions.len(),
        positions.iter().map(|p| format!(
            "<tr><td>{}</td><td>{}</td><td class=num>{}</td><td class=num>{:.3}</td></tr>",
            p.thscode, p.name, p.quantity, p.avg_cost
        )).collect::<Vec<_>>().join(""),
        decisions.iter().map(|d| format!(
            "<tr><td>{}</td><td>{}</td><td>{}</td></tr>",
            fmt_ms(d.ts_ms), d.status, esc(&d.note)
        )).collect::<Vec<_>>().join(""),
    );
    layout("概览", &body)
}

async fn positions_page(State(st): State<WebState>) -> impl IntoResponse {
    let db = st.db.lock().unwrap();
    let positions = db.positions().unwrap_or_default();
    let body = positions.iter().map(|p| format!(
        "<tr><td>{}</td><td>{}</td><td class=num>{}</td><td class=num>{:.3}</td></tr>",
        p.thscode, p.name, p.quantity, p.avg_cost
    )).collect::<Vec<_>>().join("");
    layout("持仓", &format!(
        "<h2>持仓（{} 只）</h2><table><tr><th>代码</th><th>名称</th><th>数量</th><th>成本</th></tr>{}</table>",
        positions.len(), body
    ))
}

async fn trades_page(State(st): State<WebState>) -> impl IntoResponse {
    let db = st.db.lock().unwrap();
    let trades = db.recent_trades(50).unwrap_or_default();
    let body = trades.iter().map(|t| format!(
        "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td class=num>{}</td>
        <td class=num>{:.2}</td><td class=num>{:.2}</td><td class=num>{:.2}</td></tr>",
        fmt_ms(t.ts_ms), t.thscode, t.name, t.side.as_str(), t.quantity, t.price, t.amount, t.fee
    )).collect::<Vec<_>>().join("");
    layout("流水", &format!(
        "<h2>成交流水（最近 {} 笔）</h2>
        <table><tr><th>时间</th><th>代码</th><th>名称</th><th>方向</th><th>数量</th>
        <th>价格</th><th>金额</th><th>费用</th></tr>{}</table>",
        trades.len(), body
    ))
}

async fn decisions_page(State(st): State<WebState>) -> impl IntoResponse {
    let db = st.db.lock().unwrap();
    let decisions = db.recent_decision_logs(50).unwrap_or_default();
    let body = decisions.iter().map(|d| format!(
        "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
        fmt_ms(d.ts_ms), d.model, d.status, esc(&d.note), esc(&d.raw_response.chars().take(120).collect::<String>())
    )).collect::<Vec<_>>().join("");
    layout("决策日志", &format!(
        "<h2>AI 决策日志（最近 {} 条）</h2>
        <table><tr><th>时间</th><th>模型</th><th>状态</th><th>备注</th><th>原始输出(截断)</th></tr>{}</table>",
        decisions.len(), body
    ))
}

#[derive(Deserialize)]
struct ConfigForm {
    initial_capital: f64,
    screen_top_n: u32,
    collect_interval_seconds: u64,
    ai_decision_interval_minutes: u64,
    llm_base_url: String,
    llm_model: String,
    watchlist: String,
}

async fn config_page(State(st): State<WebState>) -> impl IntoResponse {
    let c = &st.cfg;
    let body = format!(
        r#"<h2>配置（保存后写入 .env，重启生效）</h2>
        <form method=post>
        <table><tr><td>初始资金</td><td><input name=initial_capital value="{0}" type=number step=1000></td></tr>
        <tr><td>初筛 Top N</td><td><input name=screen_top_n value="{1}" type=number></td></tr>
        <tr><td>采集间隔(秒)</td><td><input name=collect_interval_seconds value="{2}" type=number></td></tr>
        <tr><td>决策间隔(分钟)</td><td><input name=ai_decision_interval_minutes value="{3}" type=number></td></tr>
        <tr><td>LLM Base URL</td><td><input name=llm_base_url value="{4}" style=width:320px></td></tr>
        <tr><td>LLM 模型</td><td><input name=llm_model value="{5}" style=width:200px></td></tr>
        <tr><td>自选池(逗号分隔)</td><td><input name=watchlist value="{6}" style=width:320px></td></tr>
        </table><p><button type=submit>保存</button></p></form>"#,
        c.initial_capital, c.screen_top_n, c.collect_interval_seconds,
        c.ai_decision_interval_minutes, esc(&c.llm_base_url), esc(&c.llm_model), esc(&c.watchlist.join(","))
    );
    layout("配置", &body)
}

async fn save_config(
    State(st): State<WebState>,
    Form(form): Form<ConfigForm>,
) -> Result<Redirect, StatusCode> {
    // 写回 .env（不覆盖密钥）
    write_env(
        &st.cfg.db_path,
        &[
            ("INITIAL_CAPITAL", &form.initial_capital.to_string()),
            ("SCREEN_TOP_N", &form.screen_top_n.to_string()),
            ("COLLECT_INTERVAL_SECONDS", &form.collect_interval_seconds.to_string()),
            ("AI_DECISION_INTERVAL_MINUTES", &form.ai_decision_interval_minutes.to_string()),
            ("LLM_BASE_URL", &form.llm_base_url),
            ("LLM_MODEL", &form.llm_model),
            ("WATCHLIST", &form.watchlist),
        ],
    )
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Redirect::to("/config"))
}

/// 更新 .env 中的指定键（保留其他行与注释）。
fn write_env(db_path: &str, kv: &[(&str, &str)]) -> std::io::Result<()> {
    let env_path = ".env";
    let content = std::fs::read_to_string(env_path).unwrap_or_default();
    let mut lines: Vec<String> = content.lines().map(|l| l.to_string()).collect();
    for (key, val) in kv {
        let line = format!("{key}={val}");
        if let Some(idx) = lines.iter().position(|l| l.starts_with(&format!("{key}="))) {
            lines[idx] = line;
        } else {
            lines.push(line);
        }
    }
    std::fs::write(env_path, lines.join("\n") + "\n")?;
    let _ = db_path;
    Ok(())
}

fn fmt_ms(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "-".into())
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}
