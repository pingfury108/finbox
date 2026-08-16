# finbox

AI 选股的 A 股模拟交易系统：分钟级抓取真实行情落库，LLM（DeepSeek）自动选股模拟买卖，全程记录决策理由并复盘验证。

## 架构

- **collector** — AkShare 实时行情，交易时段每分钟落库
- **engine** — 模拟交易引擎，成交价 = 真实行情价（钱是假的，价格是真的）
- **decision** — OpenAI 兼容接口（默认 DeepSeek），每 30 分钟决策一次，输入上下文 + 原始输出 + 动作全留痕
- **review** — 每日收盘账户快照；决策 1 天 / 5 天后自动验证对错
- **web** — FastAPI + Jinja2：概览 / 交易记录 / AI 决策日志
- **db** — SQLite + Alembic 迁移，改表不丢历史数据

## 快速开始

```bash
uv sync
cp .env.example .env   # 填入 LLM_API_KEY，按需修改 WATCHLIST
uv run alembic upgrade head
uv run uvicorn finbox.main:app --host 0.0.0.0 --port 8000
```

已初始化过数据库的，拉取新代码后执行 `uv run alembic upgrade head` 即可，历史数据不丢失。

打开 http://localhost:8000

## 说明

- AI 面向**全市场**选股：每轮决策先对全 A 快照初筛（涨幅/量比/60日涨幅 各取 Top N），AI 从候选 + 持仓中精选；新候选自动回填日线历史
- 自选池 `WATCHLIST` 可选；模拟交易**严格限定真实交易时段**下单（工作日 9:30-11:30 / 13:00-15:00，未排除法定节假日）
- 非交易时段若当天还没采过数据，会自动补一次最近收盘价（首次启动即生效）；手动「立即决策」非交易时段只记录分析、不下单
- 启动时自动补齐最近 HISTORY_DAYS 天日线历史（默认 250，前复权），供 AI 分析趋势；之后每交易日 9:15 刷新
- AI 只会交易 `WATCHLIST` 自选池 + 已持仓的股票
- 未配置 `LLM_API_KEY` 时照常采集行情，AI 决策会被跳过并记录
- 数据库变更：`uv run alembic revision --autogenerate -m "xxx"` 后 `uv run alembic upgrade head`
