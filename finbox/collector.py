"""行情采集：分钟级落库。东财为主，失败自动降级新浪，带重试"""

import logging
import time
from datetime import date, datetime, timedelta

import requests
from sqlalchemy import select
from sqlalchemy.orm import Session

from .models import DailyBar, Quote

logger = logging.getLogger(__name__)

_HEADERS = {
    "User-Agent": "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) Chrome/126.0 Safari/537.36",
    "Referer": "https://finance.sina.com.cn",
}


def _prefixed(symbol: str) -> str:
    """600519 -> sh600519"""
    if symbol.startswith(("6", "9")):
        return "sh" + symbol
    if symbol.startswith(("4", "8")):
        return "bj" + symbol
    return "sz" + symbol


def _retry(fn, tries: int = 3, delay: float = 2.0):
    for i in range(tries):
        try:
            return fn()
        except Exception:
            if i == tries - 1:
                raise
            time.sleep(delay * (i + 1))


# ---------- 实时快照 ----------

def collect_quotes(session: Session, symbols: list[str]) -> int:
    if not symbols:
        return 0
    try:
        count = _retry(lambda: _collect_eastmoney(session, symbols))
    except Exception:
        logger.warning("eastmoney spot failed, fallback to sina")
        count = _retry(lambda: _collect_sina(session, symbols))
    logger.info("collected %d quotes", count)
    return count


def _collect_eastmoney(session: Session, symbols: list[str]) -> int:
    import akshare as ak  # 延迟导入，启动快

    df = ak.stock_zh_a_spot_em()
    df = df[df["代码"].isin(symbols)]
    now = datetime.now()
    count = 0
    for _, row in df.iterrows():
        price = row.get("最新价")
        if price is None or price != price:  # NaN（停牌等）
            continue
        session.add(
            Quote(
                symbol=str(row["代码"]), name=str(row["名称"]), ts=now,
                price=float(price), pct_change=_f(row.get("涨跌幅")),
                volume=_f(row.get("成交量")), amount=_f(row.get("成交额")),
            )
        )
        count += 1
    return count


def _collect_sina(session: Session, symbols: list[str]) -> int:
    codes = ",".join(_prefixed(s) for s in symbols)
    r = requests.get(f"https://hq.sinajs.cn/list={codes}", headers=_HEADERS, timeout=10)
    r.encoding = "gbk"
    now = datetime.now()
    count = 0
    for line in r.text.strip().splitlines():
        head, _, payload = line.partition('="')
        f = payload.rstrip('";\n').split(",")
        if len(f) < 32 or not f[3]:
            continue
        cur, prev = float(f[3]), float(f[2])
        if cur == 0:  # 停牌
            continue
        try:
            ts = datetime.strptime(f"{f[30]} {f[31]}", "%Y-%m-%d %H:%M:%S")
        except ValueError:
            ts = now
        session.add(
            Quote(
                symbol=head.removeprefix("var hq_str_")[-6:], name=f[0], ts=ts,
                price=cur, pct_change=round((cur / prev - 1) * 100, 2) if prev else None,
                volume=_f(f[8]), amount=_f(f[9]),
            )
        )
        count += 1
    return count


def live_quote(symbol: str) -> dict | None:
    """实时查询任意一只股票（新浪源，不落库）"""
    try:
        r = requests.get(
            f"https://hq.sinajs.cn/list={_prefixed(symbol)}", headers=_HEADERS, timeout=8
        )
        r.encoding = "gbk"
        _, _, payload = r.text.strip().partition('="')
        f = payload.rstrip('";\n').split(",")
        if len(f) < 32 or not f[3] or float(f[3]) == 0:
            return None
        cur, prev = float(f[3]), float(f[2])
        return {
            "name": f[0], "price": cur,
            "pct": round((cur / prev - 1) * 100, 2) if prev else None,
            "volume": _f(f[8]), "amount": _f(f[9]), "time": f"{f[30]} {f[31]}",
        }
    except Exception:
        logger.exception("live_quote failed for %s", symbol)
        return None


# ---------- 日线历史 ----------

def backfill_daily_history(session: Session, symbols: list[str], days: int) -> int:
    """补齐日线历史（前复权），只插入缺失日期"""
    start = date.today() - timedelta(days=days * 2)
    count = 0
    for symbol in symbols:
        try:
            rows = _retry(lambda: _hist_eastmoney(symbol, start))
        except Exception:
            logger.warning("eastmoney hist failed for %s, fallback to sina", symbol)
            try:
                rows = _retry(lambda: _hist_sina(symbol, start))
            except Exception:
                logger.exception("backfill failed for %s", symbol)
                continue
        existing = set(
            session.scalars(select(DailyBar.date).where(DailyBar.symbol == symbol)).all()
        )
        for row in rows:
            if row["date"] in existing:
                continue
            session.add(DailyBar(symbol=symbol, **row))
            count += 1
    session.flush()
    logger.info("backfilled %d daily bars", count)
    return count


def _hist_eastmoney(symbol: str, start: date) -> list[dict]:
    import akshare as ak

    df = ak.stock_zh_a_hist(
        symbol=symbol, period="daily",
        start_date=start.strftime("%Y%m%d"), end_date=date.today().strftime("%Y%m%d"),
        adjust="qfq",
    )
    return [
        {
            "date": date.fromisoformat(str(r["日期"])),
            "open": float(r["开盘"]), "high": float(r["最高"]),
            "low": float(r["最低"]), "close": float(r["收盘"]),
            "volume": _f(r.get("成交量")), "amount": _f(r.get("成交额")),
        }
        for _, r in df.iterrows()
    ]


def _hist_sina(symbol: str, start: date) -> list[dict]:
    import akshare as ak

    df = ak.stock_zh_a_daily(symbol=_prefixed(symbol), adjust="qfq")
    rows = []
    for _, r in df.iterrows():
        d = r["date"].date() if hasattr(r["date"], "date") else date.fromisoformat(str(r["date"]))
        if d < start:
            continue
        rows.append(
            {
                "date": d,
                "open": float(r["open"]), "high": float(r["high"]),
                "low": float(r["low"]), "close": float(r["close"]),
                "volume": _f(r.get("volume")), "amount": _f(r.get("amount")),
            }
        )
    return rows


def _f(v) -> float | None:
    try:
        return None if v != v else float(v)
    except (TypeError, ValueError):
        return None
