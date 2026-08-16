"""A 股交易时段判断（未排除法定节假日）"""

from datetime import datetime, time


def is_trading_time(now: datetime | None = None) -> bool:
    now = now or datetime.now()
    if now.weekday() >= 5:
        return False
    t = now.time()
    return time(9, 30) <= t <= time(11, 30) or time(13, 0) <= t <= time(15, 0)
