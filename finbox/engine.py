"""模拟交易引擎：用真实行情价格成交，维护账户/持仓/流水"""

from datetime import datetime

from sqlalchemy import select
from sqlalchemy.orm import Session

from . import config
from .market import is_trading_time
from .models import Account, Position, Quote, Trade


def get_account(session: Session) -> Account:
    account = session.get(Account, 1)
    if account is None:
        account = Account(id=1, cash=config.INITIAL_CAPITAL, initial_capital=config.INITIAL_CAPITAL)
        session.add(account)
        session.flush()
    return account


def latest_prices(session: Session, symbols: list[str]) -> dict[str, float]:
    """每只股票最新一条行情价"""
    prices: dict[str, float] = {}
    for symbol in symbols:
        q = session.scalar(
            select(Quote).where(Quote.symbol == symbol).order_by(Quote.ts.desc()).limit(1)
        )
        if q:
            prices[symbol] = q.price
    return prices


def buy(
    session: Session,
    symbol: str,
    name: str,
    price: float,
    quantity: int,
    decision_id: int | None = None,
) -> Trade:
    if not is_trading_time():
        raise ValueError("非交易时段，禁止下单")
    if quantity <= 0 or quantity % config.LOT_SIZE != 0:
        raise ValueError(f"买入数量须为 {config.LOT_SIZE} 的整数倍: {quantity}")
    amount = round(price * quantity, 2)
    account = get_account(session)
    if amount > account.cash:
        raise ValueError(f"资金不足: 需要 {amount}, 可用 {account.cash:.2f}")

    account.cash = round(account.cash - amount, 2)
    position = session.scalar(select(Position).where(Position.symbol == symbol))
    if position:
        total_qty = position.quantity + quantity
        position.avg_cost = round(
            (position.avg_cost * position.quantity + amount) / total_qty, 4
        )
        position.quantity = total_qty
        position.updated_at = datetime.now()
    else:
        position = Position(
            symbol=symbol, name=name, quantity=quantity, avg_cost=price, updated_at=datetime.now()
        )
        session.add(position)

    trade = Trade(
        symbol=symbol, name=name, side="BUY", price=price,
        quantity=quantity, amount=amount, decision_id=decision_id,
    )
    session.add(trade)
    session.flush()
    return trade


def sell(
    session: Session,
    symbol: str,
    price: float,
    quantity: int,
    decision_id: int | None = None,
) -> Trade:
    if not is_trading_time():
        raise ValueError("非交易时段，禁止下单")
    if quantity <= 0 or quantity % config.LOT_SIZE != 0:
        raise ValueError(f"卖出数量须为 {config.LOT_SIZE} 的整数倍: {quantity}")
    position = session.scalar(select(Position).where(Position.symbol == symbol))
    if not position or position.quantity < quantity:
        raise ValueError(f"持仓不足: {symbol} 持有 {position.quantity if position else 0}")

    amount = round(price * quantity, 2)
    account = get_account(session)
    account.cash = round(account.cash + amount, 2)

    position.quantity -= quantity
    position.updated_at = datetime.now()
    if position.quantity == 0:
        session.delete(position)

    trade = Trade(
        symbol=symbol, name=position.name if position else symbol, side="SELL",
        price=price, quantity=quantity, amount=amount, decision_id=decision_id,
    )
    session.add(trade)
    session.flush()
    return trade
