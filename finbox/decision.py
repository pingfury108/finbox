"""AI 决策引擎：全市场初筛 → AI 精选 → 下单（仅交易时段），全程留痕"""

import json
import logging
from datetime import date, datetime

from openai import OpenAI
from sqlalchemy import func, select
from sqlalchemy.orm import Session

from . import config, engine, screening
from .market import is_trading_time
from .models import AIDecision, DailyBar, Position, Quote, Review, Screening, Trade

logger = logging.getLogger(__name__)

SYSTEM_PROMPT = """你是一个 A 股职业交易员，正在管理一个真实资金账户，每一笔交易都是真金白银。根据给出的账户、持仓和全市场初筛候选，决定本轮操作。

规则：
1. 可交易范围 = 当前持仓 + 今日候选（全市场初筛结果） + 自选池（如有）
2. 买卖数量必须是 100 的整数倍
3. 买入金额不能超过可用现金（含佣金等费用）
4. T+1 规则：当天买入的股票当天不能卖出
5. 涨停的股票买不进、跌停的卖不出（主板±10%，创业板/科创板±20%），接近涨跌停的谨慎操作
6. 每次交易有费用（佣金+印花税），频繁倒手会侵蚀收益
7. 候选股重点看：入选原因、量比/换手是否放大、趋势位置；优中选优，不要撒胡椒面
8. 这是真实资金，亏损是真实的：控制风险，不要满仓单只股票，没有把握就 hold，不操作也是合法决策
9. 复盘你的历史决策：被验证错误的判断要总结教训并调整，验证有效的思路可以延续
10. 如果你对池外某只股票有明确判断，可在 nominate 中提名（最多 2 只），系统会验证其真实数据，合格后下一轮起可交易
11. 你可以用 screen_focus 建议下一轮初筛的侧重方向（\"涨幅\"/\"量比\"/\"趋势\" 三选一，可选）
12. 严格输出 JSON，不要输出其他内容：
{"actions": [{"action": "buy"|"sell"|"hold", "symbol": "代码", "quantity": 数量, "reason": "理由"}], "nominate": [{"symbol": "代码", "reason": "理由"}], "screen_focus": "量比", "comment": "整体判断"}
"""


def _intraday_summary(session: Session, symbol: str) -> str:
    """当日分时特征：振幅 + 当前所处日内区间位置"""
    qs = session.scalars(
        select(Quote)
        .where(Quote.symbol == symbol, func.date(Quote.ts) == date.today())
        .order_by(Quote.ts)
    ).all()
    if len(qs) < 5:
        return ""
    prices = [q.price for q in qs]
    hi, lo, last = max(prices), min(prices), prices[-1]
    prev = engine._prev_close(session, symbol)
    if not prev or hi == lo:
        return ""
    amp = (hi - lo) / prev * 100
    pos = (last - lo) / (hi - lo)
    where = "日内高点附近" if pos > 0.8 else "日内低点附近" if pos < 0.2 else "区间中部"
    return f"今日振幅{amp:.1f}%, 当前位于{where}"


def _trend_summary(session: Session, symbol: str) -> str:
    """近 60 根日线的趋势摘要：涨跌幅、均线、区间高低点"""
    bars = session.scalars(
        select(DailyBar)
        .where(DailyBar.symbol == symbol)
        .order_by(DailyBar.date.desc())
        .limit(60)
    ).all()
    if len(bars) < 5:
        return "历史数据不足"
    closes = [b.close for b in reversed(bars)]  # 时间正序
    cur = closes[-1]
    ma20 = sum(closes[-20:]) / min(20, len(closes))
    ma60 = sum(closes) / len(closes)
    chg20 = (cur / closes[-21] - 1) * 100 if len(closes) > 20 else 0.0
    hi, lo = max(b.high for b in bars), min(b.low for b in bars)
    return (
        f"近20日{chg20:+.1f}%, MA20={ma20:.2f}, MA60={ma60:.2f}, "
        f"60日区间 {lo:.2f}~{hi:.2f}"
    )


def _latest_names(session: Session, symbols: list[str]) -> dict[str, str]:
    names: dict[str, str] = {}
    for symbol in symbols:
        n = session.scalar(
            select(Quote.name).where(Quote.symbol == symbol).order_by(Quote.ts.desc()).limit(1)
        )
        if n:
            names[symbol] = n
    return names


def _market_overview() -> str:
    """大盘指数快照（腾讯源）"""
    try:
        from .collector import _http

        r = _http.get("https://qt.gtimg.cn/q=sh000001,sz399001,sz399006", timeout=8)
        r.encoding = "gbk"
        parts = []
        for line in r.text.split(";"):
            _, _, payload = line.strip().partition('="')
            f = payload.rstrip('"').split("~")
            if len(f) > 33 and f[3]:
                parts.append(f"{f[1]} {f[3]} ({f[32]}%)")
        return " | ".join(parts) or "获取失败"
    except Exception:
        return "获取失败"


def _feedback_lines(session: Session, limit: int = 5) -> list[str]:
    """近期已执行决策 + 复盘结果，供 AI 自我修正"""
    ds = session.scalars(
        select(AIDecision)
        .where(AIDecision.status == "executed")
        .order_by(AIDecision.ts.desc())
        .limit(limit)
    ).all()
    lines = []
    for d in reversed(ds):
        trades = session.scalars(select(Trade).where(Trade.decision_id == d.id)).all()
        reviews = session.scalars(select(Review).where(Review.decision_id == d.id)).all()
        acts = ", ".join(f"{t.side} {t.symbol}{t.name}@{t.price:.2f}" for t in trades)
        fb = "; ".join(f"{r.days_after}天后浮动{r.pnl:+.0f}元" for r in reviews) or "未到复盘期"
        lines.append(f"{d.ts:%m-%d %H:%M} {acts} → {fb}")
    return lines


def _build_context(
    session: Session, candidates: list[Screening]
) -> tuple[str, dict[str, float], dict[str, str]]:
    """组装上下文，返回 (文本, 最新价, 股票名称)"""
    account = engine.get_account(session)
    positions = session.scalars(select(Position)).all()

    watch = sorted({*config.WATCHLIST, *(p.symbol for p in positions)})
    cand_symbols = [c.symbol for c in candidates]
    symbols = sorted(set(watch + cand_symbols))

    prices = engine.latest_prices(session, symbols)
    cand_map = {c.symbol: c for c in candidates}
    for c in candidates:  # 候选刚筛出可能还没分钟数据，用初筛快照价兜底
        if c.symbol not in prices:
            prices[c.symbol] = json.loads(c.metrics).get("price") or 0
    names = _latest_names(session, symbols)
    for c in candidates:
        names.setdefault(c.symbol, c.name)
    for p in positions:
        names.setdefault(p.symbol, p.name)

    lines = [
        f"时间: {datetime.now():%Y-%m-%d %H:%M}",
        f"可用现金: {account.cash:.2f} 元",
        f"大盘: {_market_overview()}",
        "",
    ]
    lines.append("== 当前持仓 ==")
    if positions:
        for p in positions:
            cur = prices.get(p.symbol)
            pnl = f"{(cur / p.avg_cost - 1) * 100:+.2f}%" if cur else "无行情"
            lines.append(
                f"{p.symbol} {p.name}: {p.quantity}股, 成本 {p.avg_cost:.2f}, "
                f"现价 {cur or '-'}, 盈亏 {pnl}"
            )
    else:
        lines.append("（空仓）")

    if watch:
        lines.append("")
        lines.append("== 自选池行情与趋势 ==")
        for s in watch:
            price = prices.get(s)
            intraday = _intraday_summary(session, s)
            lines.append(
                f"{s} {names.get(s, '')}: 最新价 {price if price else '无数据'} | "
                f"{_trend_summary(session, s)}"
                + (f" | {intraday}" if intraday else "")
            )

    if candidates:
        lines.append("")
        lines.append("== 今日全市场初筛候选 ==")
        for c in candidates:
            m = json.loads(c.metrics)
            cap = m.get("mktcap")
            cap_str = f", 市值 {cap / 1e8:.0f}亿" if cap else ""
            intraday = _intraday_summary(session, c.symbol)
            lines.append(
                f"{c.symbol} {c.name}: 现价 {m.get('price')}, 涨幅 {m.get('pct')}%, "
                f"量比 {m.get('volume_ratio')}, 换手 {m.get('turnover')}%, "
                f"60日涨幅 {m.get('chg60')}%, PE {m.get('pe')}, PB {m.get('pb')}{cap_str} | "
                f"入选: {c.reason} | {_trend_summary(session, c.symbol)}"
                + (f" | {intraday}" if intraday else "")
            )

    feedback = _feedback_lines(session)
    if feedback:
        lines.append("")
        lines.append("== 近期决策与复盘（你的历史表现，用于自我修正）==")
        lines.extend(feedback)
    return "\n".join(lines), prices, names


def _parse_actions(raw: str) -> dict:
    text = raw.strip()
    if text.startswith("```"):
        text = text.strip("`")
        if text.startswith("json"):
            text = text[4:]
    return json.loads(text)


def _handle_nomination(session: Session, symbol: str, reason: str) -> str:
    """处理 AI 提名：验证真实性 → 回填历史 → 入候选池（下轮可交易）"""
    from .collector import backfill_daily_history, live_quote

    if not (symbol.isdigit() and len(symbol) == 6):
        return f"提名 {symbol or '(空)'} 无效代码，忽略"
    pool = {c.symbol for c in screening.today_candidates(session)}
    if symbol in pool:
        return f"提名 {symbol} 已在池内，无需提名"
    live = live_quote(symbol)
    if not live:
        return f"提名 {symbol} 无行情（不存在或停牌），忽略"
    backfill_daily_history(session, [symbol], config.HISTORY_DAYS)
    session.add(
        Screening(
            ts=datetime.now(), symbol=symbol, name=live["name"],
            reason=f"AI提名: {reason[:50]}",
            metrics=json.dumps(
                {"price": live["price"], "pct": live["pct"], "volume_ratio": None,
                 "turnover": None, "chg60": None, "pe": None, "pb": None, "mktcap": None},
                ensure_ascii=False,
            ),
        )
    )
    session.flush()  # 让后续提名/本轮逻辑能看到
    return f"提名 {symbol} {live['name']} 验证通过，已入池下轮可交易"


def _fresh_price(session: Session, symbol: str) -> float | None:
    """下单用最新价：5 分钟内的采集快照，否则实时拉一次（新浪）"""
    from .collector import live_quote

    q = session.scalar(
        select(Quote).where(Quote.symbol == symbol).order_by(Quote.ts.desc()).limit(1)
    )
    if q and (datetime.now() - q.ts).total_seconds() < 300:
        return q.price
    live = live_quote(symbol)
    return live["price"] if live else None


def run_decision(session: Session) -> AIDecision:
    """一轮决策：初筛（交易时段）→ 问 LLM → 解析 → 下单 → 留痕"""
    if is_trading_time():
        candidates = screening.run_screening(session)
    else:
        candidates = screening.today_candidates(session)
    context, prices, names = _build_context(session, candidates)

    decision = AIDecision(model=config.LLM_MODEL, context=context, status="error")

    if not config.LLM_API_KEY:
        decision.note = "未配置 LLM_API_KEY，跳过"
        decision.status = "rejected"
        session.add(decision)
        session.flush()
        return decision

    # 注意：LLM 网络调用期间不能持有数据库写事务（SQLite 单写者）
    client = OpenAI(base_url=config.LLM_BASE_URL, api_key=config.LLM_API_KEY, timeout=600)
    try:
        resp = client.chat.completions.create(
            model=config.LLM_MODEL,
            messages=[
                {"role": "system", "content": SYSTEM_PROMPT},
                {"role": "user", "content": context},
            ],
            temperature=0.2,
        )
        raw = resp.choices[0].message.content or ""
        decision.raw_response = raw
        parsed = _parse_actions(raw)
    except Exception as e:
        logger.exception("LLM 调用或解析失败")
        decision.note = f"{type(e).__name__}: {e}"
        session.add(decision)
        session.flush()
        return decision

    # LLM 已返回，现在才进入写事务
    if parsed.get("screen_focus") in ("涨幅", "量比", "趋势"):
        decision.screen_focus = parsed["screen_focus"]
    session.add(decision)
    session.flush()

    actions = parsed.get("actions") or []
    decision.actions = json.dumps(actions, ensure_ascii=False)
    notes: list[str] = []
    executed = 0

    for act in actions:
        action = act.get("action")
        symbol = str(act.get("symbol", ""))
        if action == "hold" or not symbol:
            continue
        if symbol not in prices or not prices[symbol]:
            notes.append(f"{symbol} 不在股票池或无行情，跳过")
            continue
        try:
            qty = int(act.get("quantity", 0))
            # 下单前重新取最新价：LLM 响应可能耗时数分钟，上下文价格已失效
            price = _fresh_price(session, symbol) or prices[symbol]
            if not price:
                notes.append(f"{symbol} 无最新行情，跳过")
                continue
            ref = prices[symbol]
            if abs(price / ref - 1) > 0.01:
                notes.append(f"{symbol} 价格已变动 {ref:.2f} → {price:.2f}，按新价成交")
            if action == "buy":
                engine.buy(
                    session, symbol, names.get(symbol, symbol),
                    price, qty, decision_id=decision.id,
                )
            elif action == "sell":
                engine.sell(session, symbol, price, qty, decision_id=decision.id)
            else:
                continue
            executed += 1
        except (ValueError, TypeError) as e:
            notes.append(f"{action} {symbol} 被拒: {e}")

    decision.status = "executed" if executed else ("rejected" if notes else "hold")
    # AI 主动提名池外股票：验证真实数据，合格的下轮起可交易
    for nom in (parsed.get("nominate") or [])[:2]:
        notes.append(_handle_nomination(session, str(nom.get("symbol", "")), str(nom.get("reason", ""))))
    decision.note = f"comment: {parsed.get('comment', '')}; " + "; ".join(notes)
    logger.info("AI decision %s: %d executed", decision.status, executed)
    return decision
