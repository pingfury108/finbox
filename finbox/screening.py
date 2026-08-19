"""全市场初筛：东财为主、腾讯兜底。按 涨幅/量比/60日涨幅(换手) 取 Top，结果落库"""

import json
import logging
from datetime import date, datetime

from sqlalchemy import func, select
from sqlalchemy.orm import Session

from . import config
from .collector import _http, _prefixed, _retry, backfill_daily_history
from .models import AIDecision, DailyBar, Screening

logger = logging.getLogger(__name__)

_code_list_cache: tuple[date, list[str]] | None = None


_screening_source = "eastmoney"  # 进程内记忆：成功的源优先


def run_screening(session: Session) -> list[Screening]:
    """全市场初筛，候选落库并返回。双源都失败返回空列表（本轮无候选，不影响决策）

    采纳上一轮 AI 的 screen_focus 建议：侧重维度取 2N，其余取 N/2
    """
    global _screening_source
    focus = session.scalar(
        select(AIDecision.screen_focus)
        .where(AIDecision.screen_focus.isnot(None))
        .order_by(AIDecision.ts.desc())
        .limit(1)
    )
    sizes = _dim_sizes(focus)
    screeners = {"eastmoney": _screen_eastmoney, "tencent": _screen_tencent}
    order = ["eastmoney", "tencent"] if _screening_source == "eastmoney" else ["tencent", "eastmoney"]
    picks = None
    for src in order:
        try:
            picks = _retry(lambda: screeners[src](sizes)) if src == "eastmoney" else screeners[src](sizes)
            _screening_source = src
            break
        except Exception:
            logger.warning("%s screening failed, trying next source", src)
    if picks is None:
        logger.error("screening fetch failed (both sources)")
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
    """今日候选 = 最新一批全市场初筛 + 今日所有 AI 提名"""
    rows = session.scalars(
        select(Screening)
        .where(func.date(Screening.ts) == date.today())
        .order_by(Screening.ts)
    ).all()
    screened = [r for r in rows if not r.reason.startswith("AI提名")]
    nominated = [r for r in rows if r.reason.startswith("AI提名")]
    if screened:
        latest_ts = screened[-1].ts
        screened = [r for r in screened if r.ts == latest_ts]
    return screened + nominated


# ---------- 东财主源 ----------

FOCUS_DIMS = ("涨幅", "量比", "趋势")


def _dim_sizes(focus: str | None) -> dict[str, int]:
    """各维度取数：AI 侧重维度 2N，其余 N/2（下限 5）"""
    n = config.SCREEN_TOP_N
    if focus not in FOCUS_DIMS:
        return {d: n for d in FOCUS_DIMS}
    return {d: (n * 2 if d == focus else max(n // 2, 5)) for d in FOCUS_DIMS}


def _screen_eastmoney(sizes: dict[str, int]) -> dict:
    import akshare as ak  # 延迟导入

    df = ak.stock_zh_a_spot_em()
    df = df[df["最新价"].notna()]
    df = df[~df["名称"].str.contains("ST|退", na=False)]  # 排除 ST / 退市

    picks: dict[str, dict] = {}

    def take(col: str, reason: str, min_val: float | None = None, n: int = 0) -> None:
        sub = df if min_val is None else df[df[col] >= min_val]
        for _, r in sub.sort_values(col, ascending=False).head(n).iterrows():
            _pick(picks, str(r["代码"]), str(r["名称"]), reason, {
                "price": float(r["最新价"]),
                "pct": float(r["涨跌幅"]),
                "volume_ratio": _f(r["量比"]),
                "turnover": _f(r["换手率"]),
                "chg60": _f(r["60日涨跌幅"]),
                "pe": _f(r["市盈率-动态"]),
                "pb": _f(r["市净率"]),
                "mktcap": _f(r["总市值"]),
            })

    take("涨跌幅", "涨幅Top", n=sizes["涨幅"])
    take("量比", "量比Top", min_val=2.0, n=sizes["量比"])
    take("60日涨跌幅", "60日涨幅Top", n=sizes["趋势"])
    return picks


# ---------- 腾讯兜底 ----------

def _screen_tencent(sizes: dict[str, int]) -> dict:
    rows = _tencent_market_quotes()
    rows = [r for r in rows if "ST" not in r["name"] and "退" not in r["name"]]

    picks: dict[str, dict] = {}

    def take(key: str, reason: str, min_val: float | None = None, n: int = 0) -> None:
        sub = [r for r in rows if r[key] is not None]
        if min_val is not None:
            sub = [r for r in sub if r[key] >= min_val]
        for r in sorted(sub, key=lambda x: x[key], reverse=True)[:n]:
            _pick(picks, r["code"], r["name"], reason, {
                "price": r["price"], "pct": r["pct"],
                "volume_ratio": r["volume_ratio"], "turnover": r["turnover"],
                "chg60": None,  # 腾讯快照无此字段
                "pe": r["pe"], "pb": r["pb"], "mktcap": r["mktcap"],
            })

    take("pct", "涨幅Top", n=sizes["涨幅"])
    take("volume_ratio", "量比Top", min_val=2.0, n=sizes["量比"])
    take("turnover", "换手Top", min_val=5.0, n=sizes["趋势"])
    return picks


def _a_share_codes() -> list[str]:
    """全 A 代码表（交易所官方）。优先用当日新缓存，刷新失败用过期缓存兜底"""
    global _code_list_cache
    today = date.today()
    if _code_list_cache and _code_list_cache[0] == today:
        return _code_list_cache[1]
    import akshare as ak

    try:
        df = ak.stock_info_a_code_name()
        codes = [str(c) for c in df["code"]]
        _code_list_cache = (today, codes)
        return codes
    except Exception:
        if _code_list_cache:
            logger.warning("code list refresh failed, using stale cache")
            return _code_list_cache[1]
        raise


def _tencent_market_quotes() -> list[dict]:
    """全市场批量报价：腾讯接口每次 60 只，全 A 约 90 次请求；单批失败跳过不整轮作废"""
    codes = _a_share_codes()
    rows = []
    failed = 0
    for i in range(0, len(codes), 60):
        batch = ",".join(_prefixed(c) for c in codes[i : i + 60])
        try:
            r = _retry(lambda: _http.get(f"https://qt.gtimg.cn/q={batch}", timeout=10))
        except Exception:
            failed += 1
            logger.warning("tencent batch %d failed, skipped", i // 60)
            continue
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
                "pe": _f(f[39]), "mktcap": _f(f[45]), "pb": _f(f[46]),
            })
    if failed:
        logger.warning("tencent quotes: %d batches failed", failed)
    if not rows:
        raise RuntimeError("tencent quotes: all batches failed")
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
