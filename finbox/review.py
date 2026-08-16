"""复盘：验证 AI 决策在 N 天后的效果"""

import logging
from datetime import datetime, timedelta

from sqlalchemy import select
from sqlalchemy.orm import Session

from . import engine
from .models import Account, AccountSnapshot, AIDecision, Position, Review, Trade

logger = logging.getLogger(__name__)


def snapshot_account(session: Session) -> AccountSnapshot:
    """收盘账户快照"""
    account = engine.get_account(session)
    positions = session.scalars(select(Position)).all()
    prices = engine.latest_prices(session, [p.symbol for p in positions])
    market_value = sum(prices.get(p.symbol, p.avg_cost) * p.quantity for p in positions)
    snap = AccountSnapshot(
        cash=account.cash,
        market_value=round(market_value, 2),
        total_asset=round(account.cash + market_value, 2),
    )
    session.add(snap)
    session.flush()
    return snap


def review_decisions(session: Session, days_after: int = 1) -> int:
    """对 days_after 天前产生过交易的决策，用最新价格验证对错"""
    cutoff = datetime.now() - timedelta(days=days_after)
    reviewed = select(Review.decision_id).where(Review.days_after == days_after)
    decisions = session.scalars(
        select(AIDecision).where(
            AIDecision.ts <= cutoff,
            AIDecision.status == "executed",
            AIDecision.id.not_in(reviewed),
        )
    ).all()

    count = 0
    for d in decisions:
        trades = session.scalars(select(Trade).where(Trade.decision_id == d.id)).all()
        if not trades:
            continue
        prices = engine.latest_prices(session, list({t.symbol for t in trades}))
        lines, total_pnl = [], 0.0
        for t in trades:
            cur = prices.get(t.symbol)
            if cur is None:
                lines.append(f"{t.symbol} 无最新行情")
                continue
            diff = (cur - t.price) * t.quantity
            if t.side == "SELL":
                diff = -diff  # 卖出后涨跌：涨=卖飞，跌=卖对
            total_pnl += diff
            verdict = "对" if (t.side == "BUY") == (diff >= 0) else "错"
            lines.append(
                f"{t.side} {t.symbol} @ {t.price:.2f} → 现价 {cur:.2f}，"
                f"浮动 {diff:+.2f} 元，判断【{verdict}】"
            )
        session.add(
            Review(
                decision_id=d.id,
                days_after=days_after,
                summary="\n".join(lines),
                pnl=round(total_pnl, 2),
            )
        )
        count += 1
    logger.info("reviewed %d decisions (days_after=%d)", count, days_after)
    return count
