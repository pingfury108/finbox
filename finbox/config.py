import os

from dotenv import load_dotenv

load_dotenv()

DATABASE_URL = os.getenv("DATABASE_URL", "sqlite:///./finbox.db")

LLM_BASE_URL = os.getenv("LLM_BASE_URL", "https://api.deepseek.com")
LLM_API_KEY = os.getenv("LLM_API_KEY", "")
LLM_MODEL = os.getenv("LLM_MODEL", "deepseek-chat")

INITIAL_CAPITAL = float(os.getenv("INITIAL_CAPITAL", "200000"))

# 自选池（可选，留空则完全由全市场初筛驱动）
WATCHLIST = [
    s.strip()
    for s in os.getenv("WATCHLIST", "").split(",")
    if s.strip()
]

SCREEN_TOP_N = int(os.getenv("SCREEN_TOP_N", "20"))  # 初筛每个维度取前 N 只

COLLECT_INTERVAL_SECONDS = int(os.getenv("COLLECT_INTERVAL_SECONDS", "60"))
AI_DECISION_INTERVAL_MINUTES = int(os.getenv("AI_DECISION_INTERVAL_MINUTES", "30"))

HISTORY_DAYS = int(os.getenv("HISTORY_DAYS", "250"))  # 启动时补齐的日线历史长度（交易日）

LOT_SIZE = 100  # A 股一手
MAX_POSITIONS = 5  # 持股数量上限（硬护栏）
MAX_POSITION_PCT = 0.4  # 单票市值占总资产上限（硬护栏）


def _setup_no_proxy() -> None:
    """行情数据源域名绕过本机代理（Clash 等常会拦国内财经站点）"""
    if os.getenv("BYPASS_DATA_PROXY", "true").lower() not in ("1", "true", "yes"):
        return
    hosts = [
        "eastmoney.com",   # AkShare 东财行情/K线
        "sina.com.cn",     # 新浪（日线/财务）
        "sina.com",
        "sinajs.cn",       # 新浪实时行情 hq.sinajs.cn
        "gtimg.cn",        # 腾讯行情 qt.gtimg.cn
        "10jqka.com.cn",   # 同花顺
        "cninfo.com.cn",   # 巨潮
        "sse.com.cn", "szse.cn",  # 交易所
        "deepseek.com",    # 默认 LLM
    ]
    existing = os.environ.get("NO_PROXY") or os.environ.get("no_proxy") or ""
    parts = [p.strip() for p in existing.split(",") if p.strip()]
    for h in hosts:
        if h not in parts:
            parts.append(h)
    os.environ["NO_PROXY"] = ",".join(parts)
    os.environ["no_proxy"] = os.environ["NO_PROXY"]


_setup_no_proxy()
