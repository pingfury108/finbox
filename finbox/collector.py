"""行情采集：分钟级落库。东财/新浪双源互备，源记忆 + 连接池 + 重试"""

import logging
import time
from datetime import date, datetime, timedelta

import requests
from sqlalchemy import select
from sqlalchemy.orm import Session

from .models import DailyBar, Quote

logger = logging.getLogger(__name__)

# 连接池：复用 TCP/TLS，降低被风控概率也更快
_http = requests.Session()
_http.headers.update(
    {
        "User-Agent": "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) Chrome/126.0 Safari/537.36",
        "Referer": "https://finance.sina.com.cn",
    }
)


def _prefixed(symbol: str) -> str:
    """600519 -> sh600519"""
    if symbol.startswith(("6", "9")):
        return "sh" + symbol
    if symbol.startswith(("4", "8")):
        return "bj" + symbol
    return "sz" + symbol


def _retry(fn, tries: int = 2, delay: float = 2.0):
    """akshare 内部自带重试，外层 tries 不宜再大，避免故障时长时间阻塞调度"""
    for i in range(tries):
        try:
            return fn()
        except Exception:
            if i == tries - 1:
                raise
            time.sleep(delay * (i + 1))


def _with_failover(source_attr: str, fetchers: dict, label: str):
    """双源执行框架：记住上次成功的源，失败切换。返回 (结果, 源名)"""
    global _spot_source, _hist_source
    remembered = globals()[source_attr]
    order = list(fetchers) if remembered == list(fetchers)[0] else list(fetchers)[::-1]
    for src in order:
        try:
            result = _retry(fetchers[src])
            globals()[source_attr] = src
            return result, src
        except Exception:
            logger.warning("%s %s failed, trying next source", src, label)
    raise RuntimeError(f"all {label} sources failed")


# ---------- 实时快照 ----------

_spot_source = "eastmoney"


def collect_quotes(session: Session, symbols: list[str]) -> int:
    if not symbols:
        return 0
    fetchers = {
        "eastmoney": lambda: _collect_eastmoney(session, symbols),
        "sina": lambda: _collect_sina(session, symbols),
    }
    count, src = _with_failover("_spot_source", fetchers, "spot")
    logger.info("collected %d quotes via %s", count, src)
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


def _parse_sina_lines(text: str) -> list[dict]:
    """解析新浪批量行情响应"""
    rows = []
    for line in text.strip().splitlines():
        head, _, payload = line.partition('="')
        f = payload.rstrip('";\n').split(",")
        if len(f) < 32 or not f[3]:
            continue
        cur, prev = float(f[3]), float(f[2])
        if cur == 0:  # 停牌
            continue
        rows.append(
            {
                "symbol": head.removeprefix("var hq_str_")[-6:],
                "name": f[0],
                "price": cur,
                "pct": round((cur / prev - 1) * 100, 2) if prev else None,
                "volume": _f(f[8]),
                "amount": _f(f[9]),
                "time": f"{f[30]} {f[31]}",
            }
        )
    return rows


def _collect_sina(session: Session, symbols: list[str]) -> int:
    codes = ",".join(_prefixed(s) for s in symbols)
    r = _http.get(f"https://hq.sinajs.cn/list={codes}", timeout=10)
    r.encoding = "gbk"
    now = datetime.now()
    count = 0
    for row in _parse_sina_lines(r.text):
        try:
            ts = datetime.strptime(row["time"], "%Y-%m-%d %H:%M:%S")
        except ValueError:
            ts = now
        session.add(
            Quote(
                symbol=row["symbol"], name=row["name"], ts=ts,
                price=row["price"], pct_change=row["pct"],
                volume=row["volume"], amount=row["amount"],
            )
        )
        count += 1
    return count


def live_quote(symbol: str) -> dict | None:
    """实时查询任意一只股票（新浪源，不落库）"""
    try:
        r = _http.get(f"https://hq.sinajs.cn/list={_prefixed(symbol)}", timeout=8)
        r.encoding = "gbk"
        rows = _parse_sina_lines(r.text)
        if not rows:
            return None
        row = rows[0]
        return {
            "name": row["name"], "price": row["price"], "pct": row["pct"],
            "volume": row["volume"], "amount": row["amount"], "time": row["time"],
        }
    except Exception:
        logger.exception("live_quote failed for %s", symbol)
        return None


# ---------- 日线历史 ----------

_hist_source = "eastmoney"


def backfill_daily_history(session: Session, symbols: list[str], days: int) -> int:
    """补齐日线历史（前复权），只插入缺失日期。源记忆避免逐只重复重试坏源"""
    start = date.today() - timedelta(days=days * 2)
    count = 0
    for symbol in symbols:
        fetchers = {
            "eastmoney": lambda: _hist_eastmoney(symbol, start),
            "sina": lambda: _hist_sina(symbol, start),
        }
        try:
            rows, _ = _with_failover("_hist_source", fetchers, "hist")
        except RuntimeError:
            logger.error("backfill failed for %s (both sources)", symbol)
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
