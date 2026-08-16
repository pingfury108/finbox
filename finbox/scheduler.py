"""APScheduler 调度：交易时段采集行情、AI 决策；收盘后快照 + 复盘"""

import logging
from datetime import date, datetime

from apscheduler.schedulers.background import BackgroundScheduler
from sqlalchemy import select

from . import collector, config, decision, review, screening
from .db import SessionLocal
from .market import is_trading_time
from .models import Position

logger = logging.getLogger(__name__)


def _universe() -> list[str]:
    """采集池 = 自选 + 已持仓 + 今日初筛候选"""
    with SessionLocal() as s:
        held = s.scalars(select(Position.symbol)).all()
        candidates = [c.symbol for c in screening.today_candidates(s)]
    return sorted({*config.WATCHLIST, *held, *candidates})


_last_offhours_collect: date | None = None


def collect_job() -> None:
    global _last_offhours_collect
    now = datetime.now()
    offhours = not is_trading_time(now)
    # 非交易时段：数据静止（最近收盘价），每个进程每天补一次即可
    if offhours and _last_offhours_collect == now.date():
        return
    try:
        with SessionLocal() as s:
            n = collector.collect_quotes(s, _universe())
            s.commit()
        if offhours and n > 0:
            _last_offhours_collect = now.date()
    except Exception:
        logger.exception("collect_job failed")


def decision_job() -> None:
    if not is_trading_time():
        return
    try:
        with SessionLocal() as s:
            decision.run_decision(s)
            s.commit()
    except Exception:
        logger.exception("decision_job failed")


def backfill_job() -> None:
    """补齐日线历史，供 AI 分析趋势"""
    try:
        with SessionLocal() as s:
            collector.backfill_daily_history(s, _universe(), config.HISTORY_DAYS)
            s.commit()
    except Exception:
        logger.exception("backfill_job failed")


def close_job() -> None:
    """收盘：账户快照 + 复盘（1 天 / 5 天）"""
    try:
        with SessionLocal() as s:
            review.snapshot_account(s)
            review.review_decisions(s, days_after=1)
            review.review_decisions(s, days_after=5)
            s.commit()
    except Exception:
        logger.exception("close_job failed")


def start_scheduler() -> BackgroundScheduler:
    sched = BackgroundScheduler()
    sched.add_job(collect_job, "interval", seconds=config.COLLECT_INTERVAL_SECONDS, id="collect")
    sched.add_job(decision_job, "interval", minutes=config.AI_DECISION_INTERVAL_MINUTES, id="decide")
    sched.add_job(close_job, "cron", hour=15, minute=5, id="close")
    # 启动时立即补一次历史，之后每个交易日 9:15 盘前刷新
    sched.add_job(backfill_job, next_run_time=datetime.now(), id="backfill_now")
    sched.add_job(backfill_job, "cron", hour=9, minute=15, id="backfill_daily")
    sched.start()
    logger.info("scheduler started: collect=%ds, decide=%dm",
                config.COLLECT_INTERVAL_SECONDS, config.AI_DECISION_INTERVAL_MINUTES)
    return sched
