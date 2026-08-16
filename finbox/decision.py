"""AI 决策引擎：全市场初筛 → AI 精选 → 下单（仅交易时段），全程留痕"""

import json
import logging
from datetime import datetime

from openai import OpenAI
from sqlalchemy import select
from sqlalchemy.orm import Session

from . import config, engine, screening
from .market import is_trading_time
from .models import AIDecision, DailyBar, Position, Quote, Screening, Trade

logger = logging.getLogger(__name__)

SYSTEM_PROMPT = """你是一个 A 股职业交易员，正在管理一个真实资金账户，每一笔交易都是真金白银。根据给出的账户、持仓和全市场初筛候选，决定本轮操作。

规则：
1. 可交易范围 = 当前持仓 + 今日候选（全市场初筛结果） + 自选池（如有）
2. 买卖数量必须是 100 的整数倍
3. 买入金额不能超过可用现金
4. 候选股重点看：入选原因、量比/换手是否放大、趋势位置；优中选优，不要撒胡椒面
5. 这是真实资金，亏损是真实的：控制风险，不要满仓单只股票，没有把握就 hold，不操作也是合法决策
6. 严格输出 JSON，不要输出其他内容：
{"actions": [{"action": "buy"|"sell"|"hold", "symbol": "代码", "quantity": 数量, "reason": "理由"}], "comment": "整体判断"}
"""


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

    lines = [f"时间: {datetime.now():%Y-%m-%d %H:%M}", f"可用现金: {account.cash:.2f} 元", ""]
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
            lines.append(
                f"{s} {names.get(s, '')}: 最新价 {price if price else '无数据'} | "
                f"{_trend_summary(session, s)}"
            )

    if candidates:
        lines.append("")
        lines.append("== 今日全市场初筛候选 ==")
        for c in candidates:
            m = json.loads(c.metrics)
            lines.append(
                f"{c.symbol} {c.name}: 现价 {m.get('price')}, 涨幅 {m.get('pct')}%, "
                f"量比 {m.get('volume_ratio')}, 换手 {m.get('turnover')}%, "
                f"60日涨幅 {m.get('chg60')}% | 入选: {c.reason} | "
                f"{_trend_summary(session, c.symbol)}"
            )
    return "\n".join(lines), prices, names


def _parse_actions(raw: str) -> dict:
    text = raw.strip()
    if text.startswith("```"):
        text = text.strip("`")
        if text.startswith("json"):
            text = text[4:]
    return json.loads(text)


def run_decision(session: Session) -> AIDecision:
    """一轮决策：初筛（交易时段）→ 问 LLM → 解析 → 下单 → 留痕"""
    if is_trading_time():
        candidates = screening.run_screening(session)
    else:
        candidates = screening.today_candidates(session)
    context, prices, names = _build_context(session, candidates)

    decision = AIDecision(model=config.LLM_MODEL, context=context, status="error")
    session.add(decision)
    session.flush()

    if not config.LLM_API_KEY:
        decision.note = "未配置 LLM_API_KEY，跳过"
        decision.status = "rejected"
        return decision

    client = OpenAI(base_url=config.LLM_BASE_URL, api_key=config.LLM_API_KEY)
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
        return decision

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
            if action == "buy":
                engine.buy(
                    session, symbol, names.get(symbol, symbol),
                    prices[symbol], qty, decision_id=decision.id,
                )
            elif action == "sell":
                engine.sell(session, symbol, prices[symbol], qty, decision_id=decision.id)
            else:
                continue
            executed += 1
        except (ValueError, TypeError) as e:
            notes.append(f"{action} {symbol} 被拒: {e}")

    decision.status = "executed" if executed else ("rejected" if notes else "hold")
    decision.note = f"comment: {parsed.get('comment', '')}; " + "; ".join(notes)
    logger.info("AI decision %s: %d executed", decision.status, executed)
    return decision
