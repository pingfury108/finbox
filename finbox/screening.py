"""全市场初筛：东财为主、腾讯兜底。按 涨幅/量比/60日涨幅(换手) 取 Top，结果落库"""

import json
import logging
from datetime import date, datetime

import requests
from sqlalchemy import func, select
from sqlalchemy.orm import Session

from . import config
from .collector import _prefixed, _retry, backfill_daily_history
from .models import DailyBar, Screening

logger = logging.getLogger(__name__)

_HEADERS = {"User-Agent": "Mozilla/5.0", "Referer": "https://gu.qq.com/"}

_code_list_cache: tuple[date, list[str]] | None = None


def run_screening(session: Session) -> list[Screening]:
    """全市场初筛，候选落库并返回。双源都失败返回空列表（本轮无候选，不影响决策）"""
    try:
        picks = _retry(_screen_eastmoney)
    except Exception:
        logger.warning("eastmoney screening failed, fallback to tencent")
        try:
            picks = _screen_tencent()
        except Exception:
            logger.exception("screening fetch failed (both sources)")
            return []

    now = datetime.now()
    results = []
    for code, p in picks.items():
        s = Screening(
            ts=now, symbol=code, name=p["name"],
            reason="/".join(p["reasons"]), metrics=json.dumps(p["metrics"], ensure_ascii=False),
        )
        session.add(s)
        results.append(s)
    session.flush()

    # 新候选缺日线历史的，顺手回填（首次约 1-2 分钟，之后增量很快）
    missing = [
        s.symbol for s in results
        if session.scalar(
            select(func.count(DailyBar.id)).where(DailyBar.symbol == s.symbol)
        ) < 5
    ]
    if missing:
        logger.info("backfilling history for %d new candidates", len(missing))
        backfill_daily_history(session, missing, config.HISTORY_DAYS)

    logger.info("screening picked %d candidates", len(results))
    return results


def today_candidates(session: Session) -> list[Screening]:
    """今天最新一批候选"""
    latest_ts = session.scalar(select(Screening.ts).order_by(Screening.ts.desc()).limit(1))
    if not latest_ts or latest_ts.date() != datetime.now().date():
        return []
    return session.scalars(select(Screening).where(Screening.ts == latest_ts)).all()


# ---------- 东财主源 ----------

def _screen_eastmoney() -> dict:
    import akshare as ak  # 延迟导入

    df = ak.stock_zh_a_spot_em()
    df = df[df["最新价"].notna()]
    df = df[~df["名称"].str.contains("ST|退", na=False)]  # 排除 ST / 退市

    picks: dict[str, dict] = {}

    def take(col: str, reason: str, min_val: float | None = None) -> None:
        sub = df if min_val is None else df[df[col] >= min_val]
        for _, r in sub.sort_values(col, ascending=False).head(config.SCREEN_TOP_N).iterrows():
            _pick(picks, str(r["代码"]), str(r["名称"]), reason, {
                "price": float(r["最新价"]),
                "pct": float(r["涨跌幅"]),
                "volume_ratio": _f(r["量比"]),
                "turnover": _f(r["换手率"]),
                "chg60": _f(r["60日涨跌幅"]),
            })

    take("涨跌幅", "涨幅Top")
    take("量比", "量比Top", min_val=2.0)
    take("60日涨跌幅", "60日涨幅Top")
    return picks


# ---------- 腾讯兜底 ----------

def _screen_tencent() -> dict:
    rows = _tencent_market_quotes()
    rows = [r for r in rows if "ST" not in r["name"] and "退" not in r["name"]]

    picks: dict[str, dict] = {}

    def take(key: str, reason: str, min_val: float | None = None) -> None:
        sub = [r for r in rows if r[key] is not None]
        if min_val is not None:
            sub = [r for r in sub if r[key] >= min_val]
        for r in sorted(sub, key=lambda x: x[key], reverse=True)[: config.SCREEN_TOP_N]:
            _pick(picks, r["code"], r["name"], reason, {
                "price": r["price"], "pct": r["pct"],
                "volume_ratio": r["volume_ratio"], "turnover": r["turnover"],
                "chg60": None,  # 腾讯快照无此字段
            })

    take("pct", "涨幅Top")
    take("volume_ratio", "量比Top", min_val=2.0)
    take("turnover", "换手Top", min_val=5.0)
    return picks


def _a_share_codes() -> list[str]:
    """全 A 代码表（交易所官方，当日缓存）"""
    global _code_list_cache
    today = date.today()
    if _code_list_cache and _code_list_cache[0] == today:
        return _code_list_cache[1]
    import akshare as ak

    df = ak.stock_info_a_code_name()
    codes = [str(c) for c in df["code"]]
    _code_list_cache = (today, codes)
    return codes


def _tencent_market_quotes() -> list[dict]:
    """全市场批量报价：腾讯接口每次 60 只，全 A 约 90 次请求"""
    codes = _a_share_codes()
    rows = []
    for i in range(0, len(codes), 60):
        batch = ",".join(_prefixed(c) for c in codes[i : i + 60])
        r = requests.get(f"https://qt.gtimg.cn/q={batch}", headers=_HEADERS, timeout=10)
        r.encoding = "gbk"
        for line in r.text.split(";"):
            _, _, payload = line.strip().partition('="')
            f = payload.rstrip('"').split("~")
            if len(f) < 50 or not f[3]:
                continue
            price = _f(f[3])
            if not price:  # 停牌
                continue
            rows.append({
                "code": f[2], "name": f[1], "price": price,
                "pct": _f(f[32]), "turnover": _f(f[38]), "volume_ratio": _f(f[49]),
            })
    return rows


# ---------- 工具 ----------

def _pick(picks: dict, code: str, name: str, reason: str, metrics: dict) -> None:
    p = picks.setdefault(code, {"name": name, "reasons": [], "metrics": metrics})
    p["reasons"].append(reason)


def _f(v) -> float | None:
    try:
        f = float(v)
        return None if f != f else f
    except (TypeError, ValueError):
        return None
