from datetime import date, datetime

from sqlalchemy import Date, ForeignKey, Index, String, Text, UniqueConstraint
from sqlalchemy.orm import Mapped, mapped_column

from .db import Base


class Account(Base):
    """模拟账户，单行表"""

    __tablename__ = "account"

    id: Mapped[int] = mapped_column(primary_key=True)
    cash: Mapped[float]
    initial_capital: Mapped[float]


class Quote(Base):
    """分钟级行情快照（真实数据）"""

    __tablename__ = "quotes"
    __table_args__ = (Index("ix_quotes_symbol_ts", "symbol", "ts"),)

    id: Mapped[int] = mapped_column(primary_key=True)
    symbol: Mapped[str] = mapped_column(String(16))
    name: Mapped[str] = mapped_column(String(64))
    ts: Mapped[datetime]
    price: Mapped[float]
    pct_change: Mapped[float | None]
    volume: Mapped[float | None]
    amount: Mapped[float | None]


class Position(Base):
    __tablename__ = "positions"

    id: Mapped[int] = mapped_column(primary_key=True)
    symbol: Mapped[str] = mapped_column(String(16), unique=True)
    name: Mapped[str] = mapped_column(String(64))
    quantity: Mapped[int]
    avg_cost: Mapped[float]
    updated_at: Mapped[datetime] = mapped_column(default=datetime.now)


class Trade(Base):
    """成交流水，价格为真实行情价"""

    __tablename__ = "trades"

    id: Mapped[int] = mapped_column(primary_key=True)
    symbol: Mapped[str] = mapped_column(String(16))
    name: Mapped[str] = mapped_column(String(64))
    side: Mapped[str] = mapped_column(String(4))  # BUY / SELL
    price: Mapped[float]
    quantity: Mapped[int]
    amount: Mapped[float]
    fee: Mapped[float] = mapped_column(default=0.0)
    ts: Mapped[datetime] = mapped_column(default=datetime.now)
    decision_id: Mapped[int | None] = mapped_column(ForeignKey("ai_decisions.id"))


class AIDecision(Base):
    """AI 决策日志：输入上下文、完整输出、解析动作，全程留痕"""

    __tablename__ = "ai_decisions"

    id: Mapped[int] = mapped_column(primary_key=True)
    ts: Mapped[datetime] = mapped_column(default=datetime.now)
    model: Mapped[str] = mapped_column(String(64))
    context: Mapped[str] = mapped_column(Text)  # 给 AI 看的上下文（含 prompt）
    raw_response: Mapped[str] = mapped_column(Text, default="")
    actions: Mapped[str] = mapped_column(Text, default="[]")  # 解析后的 JSON
    status: Mapped[str] = mapped_column(String(16))  # executed / hold / rejected / error
    note: Mapped[str] = mapped_column(Text, default="")


class AccountSnapshot(Base):
    """每日收盘账户快照，用于收益曲线"""

    __tablename__ = "account_snapshots"

    id: Mapped[int] = mapped_column(primary_key=True)
    ts: Mapped[datetime] = mapped_column(default=datetime.now)
    cash: Mapped[float]
    market_value: Mapped[float]
    total_asset: Mapped[float]


class DailyBar(Base):
    """日线历史（前复权），供 AI 分析趋势"""

    __tablename__ = "daily_bars"
    __table_args__ = (
        UniqueConstraint("symbol", "date"),
        Index("ix_daily_bars_symbol_date", "symbol", "date"),
    )

    id: Mapped[int] = mapped_column(primary_key=True)
    symbol: Mapped[str] = mapped_column(String(16))
    date: Mapped[date] = mapped_column(Date)
    open: Mapped[float]
    high: Mapped[float]
    low: Mapped[float]
    close: Mapped[float]
    volume: Mapped[float | None]
    amount: Mapped[float | None]


class Screening(Base):
    """全市场初筛候选：每轮决策前对全 A 快照打分取 Top"""

    __tablename__ = "screenings"
    __table_args__ = (Index("ix_screenings_ts", "ts"),)

    id: Mapped[int] = mapped_column(primary_key=True)
    ts: Mapped[datetime]
    symbol: Mapped[str] = mapped_column(String(16))
    name: Mapped[str] = mapped_column(String(64))
    reason: Mapped[str] = mapped_column(String(128))  # 涨幅Top / 量比Top / 60日涨幅Top
    metrics: Mapped[str] = mapped_column(Text, default="{}")  # JSON: price/pct/volume_ratio/...


class Review(Base):
    """复盘：某次 AI 决策 N 天后的验证结果"""

    __tablename__ = "reviews"

    id: Mapped[int] = mapped_column(primary_key=True)
    decision_id: Mapped[int] = mapped_column(ForeignKey("ai_decisions.id"))
    days_after: Mapped[int]
    ts: Mapped[datetime] = mapped_column(default=datetime.now)
    summary: Mapped[str] = mapped_column(Text, default="")
    pnl: Mapped[float | None]
