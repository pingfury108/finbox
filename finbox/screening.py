"""全市场初筛：东财快照一次拿全 A，按 涨幅/量比/60日涨幅 取 Top，结果落库"""

import json
import logging
from datetime import datetime

from sqlalchemy import func, select
from sqlalchemy.orm import Session

from . import config
from .collector import _retry, backfill_daily_history
from .models import DailyBar, Screening

logger = logging.getLogger(__name__)


def run_screening(session: Session) -> list[Screening]:
    """全市场初筛，候选落库并返回。失败返回空列表（本轮无候选，不影响决策）"""
    import akshare as ak  # 延迟导入

    try:
        df = _retry(lambda: ak.stock_zh_a_spot_em())
    except Exception:
        logger.exception("screening fetch failed")
        return []

    df = df[df["最新价"].notna()]
    df = df[~df["名称"].str.contains("ST|退", na=False)]  # 排除 ST / 退市

    picks: dict[str, dict] = {}

    def take(col: str, reason: str, min_val: float | None = None) -> None:
        sub = df if min_val is None else df[df[col] >= min_val]
        for _, r in sub.sort_values(col, ascending=False).head(config.SCREEN_TOP_N).iterrows():
            code = str(r["代码"])
            p = picks.setdefault(
                code,
                {
                    "name": str(r["名称"]),
                    "reasons": [],
                    "metrics": {
                        "price": float(r["最新价"]),
                        "pct": float(r["涨跌幅"]),
                        "volume_ratio": float(r["量比"]) if r["量比"] == r["量比"] else None,
                        "turnover": float(r["换手率"]) if r["换手率"] == r["换手率"] else None,
                        "chg60": float(r["60日涨跌幅"]) if r["60日涨跌幅"] == r["60日涨跌幅"] else None,
                    },
                },
            )
            p["reasons"].append(reason)

    take("涨跌幅", "涨幅Top")
    take("量比", "量比Top", min_val=2.0)
    take("60日涨跌幅", "60日涨幅Top")

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
