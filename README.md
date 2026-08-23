# finbox

AI 选股的 A 股模拟交易系统（Rust 版）。目标：稳定小赚（5%），严格控制回撤（≤5%）。

- 数据：同花顺（hithink-sdk）全市场行情落本地 DuckDB
- 决策：每日收盘 AI（DeepSeek）从精品候选选股，决策与执行解耦
- 风控：单票止损 -5%、止盈 +15%、账户回撤 -5% 熔断，独立于 AI 不可绕过
- 执行：模拟盘严格按 A 股规则（T+1/涨跌停/整手/费用），真实行情价成交
- Web：深色行情界面，账户概览 / K线行情 / 持仓 / 交易 / AI建议 / 设置

## 架构（Cargo workspace）

```
crates/
├── hithink-sdk        同花顺 REST API SDK（信封/重试/流式下载/指数K线）
├── finbox-store       DuckDB：行情库 + 账户库（SharedDb 单连接共享）
├── finbox-core        领域模型 + A股规则（T+1/涨跌停/整手/费用/护栏）
├── finbox-collector   数据采集同步 CLI（init/sync/index/snapshot/...）
├── finbox-trader      Broker trait + SimBroker + 风控层
├── finbox-decision    全市场初筛（SQL打分）+ LLM 决策（意图解耦）
└── finbox-app         主程序（产物名 finbox）：单进程调度（采集 + 多账户 + Web）
```

数据目录：`data/market.duckdb`（共享行情）+ `data/accounts/<名>/account.duckdb`（每账户独立，互不干扰）。

## 快速开始

```bash
# 1. 初始化行情库（全市场 10 年日K + 复权 + 指数）
cp .env.example .env    # 填 HITHINK_FINANCE_API_KEY / LLM_API_KEY
cargo run -p finbox-collector -- init
cargo run -p finbox-collector -- index --days 1200

# 2. 创建模拟账户
cargo run -p finbox-app -- account create 我的账户 --capital 200000

# 3. 一键启动整个系统（采集 + 多账户调度 + Web，端口 FINBOX_BIND 默认 0.0.0.0:8000）
cargo run --release -p finbox-app -- run
```

打开 http://localhost:8000

## 说明

- **选股**：单条 SQL 全市场打分（趋势/回调/放量/位置 + 硬过滤），只出 3-5 只精品候选给 AI
- **决策**：每日收盘一次（15:05），AI 从候选精选 1-2 只，数量由系统按仓位约束计算（单票≤20%、总仓位≤60%、持仓≤3）
- **风控**（独立于 AI）：单票 -5% 强制止损；+15% 减半止盈；持仓超 20 天无起色清仓；账户回撤 -5% 熔断停买 5 天；市场走弱（涨跌家数）自动降仓位
- **复盘**：决策 1/5/10 天后验证盈亏，反馈喂回下一轮 AI 上下文
- **多账户**：每个账户独立资金/持仓/决策，单进程并行运行，互不干扰；Web 顶部切换账户
- **配置热生效**：同花顺 key / AI key / 策略参数存库 meta，Web 设置页修改即时生效，无需重启
- 行情页默认展示几大 A 股指数 K 线（上证/深成/创业板/沪深300/中证500），可搜索个股

## 环境变量

见 `.env.example`：`HITHINK_FINANCE_API_KEY`（同花顺数据）、`LLM_*`（AI 服务）、`FINBOX_DATA`（数据目录，默认 data）。
