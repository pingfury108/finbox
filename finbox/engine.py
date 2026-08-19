"""模拟交易引擎：真实行情价成交，遵循 A 股规则（T+1 / 涨跌停 / 费用 / 整手）"""

from datetime import date, datetime

from sqlalchemy import func, select
from sqlalchemy.orm import Session

from . import config
from .market import is_trading_time
from .models import Account, DailyBar, Position, Quote, Trade

# 费用：佣金万2.5（最低5元，双边）+ 印花税0.05%（卖出）+ 过户费0.001%（双边）
COMMISSION_RATE = 0.00025
COMMISSION_MIN = 5.0
STAMP_RATE = 0.0005
TRANSFER_RATE = 0.00001


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


def _fees(amount: float, side: str) -> float:
    commission = max(amount * COMMISSION_RATE, COMMISSION_MIN)
    stamp = amount * STAMP_RATE if side == "SELL" else 0.0
    return round(commission + stamp + amount * TRANSFER_RATE, 2)


def _limit_ratio(symbol: str) -> float:
    """板块涨跌幅限制：创业/科创 20%，北交所 30%，主板 10%"""
    if symbol.startswith(("300", "688")):
        return 0.20
    if symbol.startswith(("4", "8")):
        return 0.30
    return 0.10


def _prev_close(session: Session, symbol: str) -> float | None:
    """昨收价（日线表最新一根）"""
    bar = session.scalar(
        select(DailyBar).where(DailyBar.symbol == symbol).order_by(DailyBar.date.desc()).limit(1)
    )
    return bar.close if bar else None


def _today_bought(session: Session, symbol: str) -> int:
    """当日买入数量（T+1 不可卖部分）"""
    return session.scalar(
        select(func.coalesce(func.sum(Trade.quantity), 0)).where(
            Trade.symbol == symbol,
            Trade.side == "BUY",
            func.date(Trade.ts) == date.today(),
        )
    )


def _total_asset(session: Session, account: Account) -> float:
    positions = session.scalars(select(Position)).all()
    prices = latest_prices(session, [p.symbol for p in positions])
    mv = sum(prices.get(p.symbol, p.avg_cost) * p.quantity for p in positions)
    return account.cash + mv


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

    prev = _prev_close(session, symbol)
    if prev:
        limit_up = round(prev * (1 + _limit_ratio(symbol)), 2)
        if price >= limit_up:
            raise ValueError(f"{symbol} 已涨停（{price:.2f} ≥ {limit_up:.2f}），无法买入")

    amount = round(price * quantity, 2)
    fee = _fees(amount, "BUY")
    account = get_account(session)
    if amount + fee > account.cash:
        raise ValueError(f"资金不足: 需要 {amount + fee:.2f}(含费{fee:.2f}), 可用 {account.cash:.2f}")

    # 硬护栏：单票仓位 ≤ 40% 总资产，持股 ≤ 5 只
    position = session.scalar(select(Position).where(Position.symbol == symbol))
    total = _total_asset(session, account)
    if (position.quantity * price if position else 0) + amount > total * config.MAX_POSITION_PCT:
        raise ValueError(f"单票仓位超限: 买入后市值占比将超 {config.MAX_POSITION_PCT:.0%}")
    if position is None:
        held = session.scalar(select(func.count(Position.id)))
        if held >= config.MAX_POSITIONS:
            raise ValueError(f"持股数量超限: 已持有 {held} 只，上限 {config.MAX_POSITIONS} 只")

    account.cash = round(account.cash - amount - fee, 2)
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
        quantity=quantity, amount=amount, fee=fee, decision_id=decision_id,
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

    # T+1：当日买入部分不可卖
    sellable = position.quantity - _today_bought(session, symbol)
    if quantity > sellable:
        raise ValueError(f"T+1 限制: {symbol} 可卖 {sellable} 股（当日买入部分次日可卖）")

    prev = _prev_close(session, symbol)
    if prev:
        limit_down = round(prev * (1 - _limit_ratio(symbol)), 2)
        if price <= limit_down:
            raise ValueError(f"{symbol} 已跌停（{price:.2f} ≤ {limit_down:.2f}），无法卖出")

    amount = round(price * quantity, 2)
    fee = _fees(amount, "SELL")
    account = get_account(session)
    account.cash = round(account.cash + amount - fee, 2)

    name = position.name
    position.quantity -= quantity
    position.updated_at = datetime.now()
    if position.quantity == 0:
        session.delete(position)

    trade = Trade(
        symbol=symbol, name=name, side="SELL", price=price,
        quantity=quantity, amount=amount, fee=fee, decision_id=decision_id,
    )
    session.add(trade)
    session.flush()
    return trade
