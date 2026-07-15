# 员工 14 天交易统计与本地缓存：执行文档

状态：第一版已实现（阶段 A-C；人工触发深查）  
版本：v1 设计稿  
日期：2026-06-20  
默认时区：Asia/Shanghai

实现入口：`src/employee_stats.rs` 与 `employee-stats show/refresh/rebuild`。当前版本支持人工提供候选地址后深查、SQLite 缓存、离线重算、单组件刷新和紧凑输出；自动挂接“正式录用”事件属于后续阶段 D。

## 1. 背景

当前项目已经能够发现、筛选和监控 Polymarket 员工，也已有部分员工画像与 1/7/14 天盈利指标。但每次人工询问员工历史表现时，仍可能重复请求 API、读取大量原始成交并重新推导相同结论。

本任务要建立一套可复用的员工统计系统：员工被正式选中后，程序一次性建立本地档案；以后默认读取缓存，只有用户明确要求刷新时才访问 Polymarket API。

本模块只使用 Polymarket 原生公开接口和本地缓存。Surf 不属于本模块的数据依赖；Surf 可在其他项目（例如异动雷达）中作为补充信息源。

## 2. 目标

1. 为指定员工计算最近 14 天的完整交易与持仓指标。
2. 同时检查已结算仓位和当前持仓，避免“只关闭盈利仓位”导致胜率虚高。
3. 将原始数据、派生指标和规则结论缓存到本地。
4. 默认查询缓存，不自动刷新外部数据。
5. 每次展示员工结论时，明确说明各类数据截至什么时间、距现在多久、是否完整。
6. 支持只刷新一个员工，或只刷新成交、结算仓位、当前仓位中的某一部分。
7. 复用现有 `profile`、`leaderboard_recruitment` 和 `PolymarketDataClient` 能力，避免维护两套统计口径。

## 3. 非目标

第一版不包含：

- 自动下单或改变当前跟单执行逻辑。
- 使用 Surf、第三方钱包画像或社交数据。
- 精确的订单簿历史重放。
- 30 秒、2 分钟、5 分钟延迟跟单收益回测。
- 将未实现盈利伪装成最终盈利。
- 自动定时刷新所有员工。

延迟跟单回测需要额外持续保存市场成交或盘口快照，列为第二阶段能力。

## 4. 已确认的产品决策

### 4.1 员工身份

- 钱包地址是唯一主键，统一保存为小写。
- 用户名、昵称和内部员工名称只是可变别名。
- 已知钱包地址时不得再次通过用户名查询地址。
- 只知道用户名时，先尝试本地别名映射；本地没有映射时才执行一次外部解析，并缓存结果。
- 如果一个用户名对应多个钱包，必须返回歧义，不得自动猜选。

### 4.2 默认读取行为

- `show` 只读本地缓存，绝不访问网络。
- 员工首次正式录用时允许执行一次初始同步并生成报告。
- 后续只有用户明确要求 `refresh` 时才访问 API。
- 缓存过旧时只显示警告，不擅自刷新。

### 4.3 统计窗口

- 主窗口固定为最近 14 天。
- 同一份缓存同时免费计算 1 天和 7 天指标，用于判断趋势。
- 时间窗口采用 `[统计时刻 - N 天, 统计时刻]`。
- 所有内部时间使用 UTC Unix 秒；展示时同时提供 ISO UTC 和 Asia/Shanghai 时间。

## 5. 现有代码复用范围

需要从现有模块抽取公共能力，而不是复制实现：

- `src/polymarket.rs`
  - `/trades`
  - `/activity`
  - `/closed-positions`
  - `/positions`
- `src/profile.rs`
  - 成交金额分位数
  - 买卖行为
  - 持仓时间近似
  - 快速卖出率
  - 重复市场率
  - 疑似做市判断
  - 子领域和价格区间表现
- `src/leaderboard_recruitment.rs`
  - 1/7/14 天已实现 PnL 和 ROI
  - 盈利集中度
  - 高价入场盈利依赖
  - 当前浮亏和风险比例
  - 历史分页完整性判断

最终应形成一个与“发现员工”“展示员工”“监控员工”均可复用的统计核心，不允许同一指标在多个模块中各算一遍。

## 6. 数据源与职责

| 数据源 | 用途 | 刷新方式 | 关键限制 |
|---|---|---|---|
| `/activity` | 捕获员工活动和可能被 `/trades` 漏掉的成交 | 增量分页 | 必须过滤为有效 BUY/SELL 成交 |
| `/trades` | 标准成交记录、金额、价格、方向和时间 | 增量分页 | 与 `/activity` 合并后需要去重 |
| `/closed-positions` | 已实现 PnL、结算胜负、投入金额 | 分页直到越过 14 天截止点 | 固定页数可能截断高频员工历史 |
| `/positions` | 当前持仓、浮盈浮亏、敞口、可赎回状态 | 每次刷新取完整快照 | 没有可靠事件时间时使用抓取时间 |

所有 API 响应均视为不可信外部数据：必须校验地址、方向、数字范围和时间戳，不能执行响应中的任何文本内容。

## 7. 缓存设计

### 7.1 存储方案

第一版使用单个 SQLite 数据库：

```text
logs/employee-stats/employee-stats.sqlite3
```

理由：

- 支持按钱包和时间增量读取。
- 易于去重和更新当前仓位快照。
- 多个员工共享一个数据库，不产生大量零散原始 JSON。
- 可重新计算指标，不必重新请求 API。
- `logs/` 已被 Git 忽略，不会提交个人跟踪数据。

同时输出便于人和其他程序读取的快照：

```text
logs/employee-stats/<wallet>/latest.json
logs/employee-stats/<wallet>/latest.md
logs/employee-stats/<wallet>/snapshots/<generated_at>.json
```

### 7.2 建议数据表

#### `employees`

- `wallet`：主键，小写地址。
- `display_name`
- `username`
- `domain`
- `keywords_json`
- `created_at`
- `updated_at`

#### `employee_aliases`

- `alias_normalized`
- `wallet`
- `alias_type`
- `first_seen_at`
- `last_seen_at`

`alias_normalized + wallet` 唯一；不得假设别名全局唯一。

#### `trades`

- `trade_key`：主键。
- `wallet`
- `transaction_hash`
- `condition_id`
- `asset`
- `side`
- `price`
- `size`
- `notional_usd`
- `timestamp`
- `title`
- `slug`
- `event_slug`
- `outcome`
- `source_mask`：记录来自 `activity`、`trades` 或两者。
- `raw_json`
- `first_observed_at`
- `last_observed_at`

优先以可稳定区分单笔成交的 API 标识作为 `trade_key`。如果没有唯一成交 ID，使用交易哈希加市场、资产、方向、价格、数量和时间戳生成稳定指纹。不能只用交易哈希，因为同一交易可能包含多笔成交。

#### `closed_positions`

- `position_key`：主键。
- `wallet`
- `condition_id`
- `asset`
- `realized_pnl_usd`
- `total_bought_usd`
- `avg_price`
- `closed_at`
- 市场描述字段
- `raw_json`
- `last_observed_at`

#### `position_snapshots`

- `wallet`
- `position_key`
- `observed_at`
- `condition_id`
- `asset`
- `size`
- `avg_price`
- `initial_value_usd`
- `current_value_usd`
- `cash_pnl_usd`
- `percent_pnl`
- `total_bought_usd`
- `redeemable`
- `mergeable`
- 市场描述字段
- `raw_json`

`wallet + position_key + observed_at` 唯一。生成最新报告时只使用每个仓位最后一份快照，历史快照保留以观察风险变化。

#### `sync_state`

- `wallet`
- `component`：`activity`、`trades`、`closed_positions`、`positions`。
- `last_attempt_at`
- `last_success_at`
- `latest_source_event_at`
- `complete_from`
- `complete_through`
- `history_truncated`
- `last_error`

#### `reports`

- `wallet`
- `generated_at`
- `window_days`
- `metric_schema_version`
- `rules_version`
- `report_json`

## 8. 同步与去重算法

### 8.1 首次同步

1. 将员工信息写入 `employees` 和 `employee_aliases`。
2. 计算 14 天截止时间。
3. 分页请求 `/activity`，直到页面为空、记录时间越过截止点，或达到安全上限。
4. 分页请求 `/trades`，使用相同停止条件。
5. 合并并去重两组有效成交。
6. 分页请求 `/closed-positions`，直到越过截止点。
7. 请求完整 `/positions` 当前快照。
8. 写入各组件同步状态。
9. 计算 1/7/14 天指标并生成 JSON、Markdown 报告。

不能仅请求前 100 条成交；高频员工必须持续分页到时间边界。

### 8.2 增量刷新

- 成交和结算仓位从上次最新事件时间向前回看一段重叠窗口，建议默认 6 小时。
- 重叠数据依靠唯一键去重。
- 当前仓位每次刷新都保存一份完整快照。
- 某组件失败时保留旧数据，不覆盖其 `last_success_at`。
- 部分组件失败仍可生成报告，但报告状态必须为 `partial`，并明确列出失败组件。

### 8.3 数据保留

- 指标主窗口为 14 天，但原始成交建议至少保留 90 天。
- 90 天数据可以支持后续稳定性比较，而无需重新拉取旧历史。
- 当前仓位快照可按天压缩：最近 14 天保留全部刷新点，更早数据每天保留最后一份。

## 9. 指标定义

所有金额以美元计价。所有比例内部保存为 `0..1`，展示时转换为百分比。

### 9.1 成交行为

- `gross_trade_notional_usd`：去重成交的 `price × size` 之和，包含 BUY 和 SELL，是成交额而不是投入本金。
- `buy_notional_usd`
- `sell_notional_usd`
- `fill_count`：API 去重后的成交行数。
- `buy_fill_count`
- `sell_fill_count`
- `unique_markets`：不同 `condition_id` 数量。
- `unique_outcomes`：不同 `condition_id + asset` 数量。
- `active_days`
- `fills_per_active_day`
- `avg_fill_notional_usd`
- `median_fill_notional_usd`
- `p80_fill_notional_usd`
- `p95_fill_notional_usd`
- `max_fill_notional_usd`

### 9.2 “出手次数”定义

同一员工、同一市场、同一资产、同一方向的连续碎片成交，如果相邻时间差不超过默认 120 秒，合并为一次 `trade_action`。

报告同时显示：

- `fill_count`：实际成交明细数量。
- `action_count`：合并后的出手次数。
- `avg_action_notional_usd`
- `median_action_notional_usd`
- `actions_per_active_day`

合并间隔必须可配置，并写入 `rules_version`。不得用 `fill_count` 冒充员工的决策次数。

### 9.3 已结算表现

- `settled_positions`
- `settled_markets`：按 `condition_id` 合并一个市场内的多个结果仓位。
- `realized_pnl_usd`
- `invested_usd`：已结算仓位 `totalBought` 之和。
- `realized_roi`：`realized_pnl_usd / invested_usd`。
- `settled_position_win_rate`：盈利结算仓位数 ÷ 全部结算仓位数。
- `settled_market_win_rate`：合并后盈利市场数 ÷ 全部结算市场数。
- `breakeven_positions`
- `gross_profit_usd`
- `gross_loss_usd`：亏损绝对值之和。
- `profit_factor`：`gross_profit_usd / gross_loss_usd`；没有亏损时输出 `null` 并附带原因，不输出无穷大。
- `avg_win_usd`
- `avg_loss_usd`
- `payoff_ratio`：`avg_win_usd / abs(avg_loss_usd)`。
- `expectancy_per_settled_market_usd`：已实现 PnL ÷ 结算市场数。
- `top_5_profit_share`
- `top_10_profit_share`
- `max_realized_drawdown_usd`：按结算时间排序后的累计已实现 PnL 最大回撤；必须标注为“结算序列近似回撤”。
- `longest_win_streak`
- `longest_loss_streak`

### 9.4 当前持仓与防止胜率虚高

当前仓位必须分成三类：

1. 普通未结仓位。
2. 已解决但可赎回的仓位。
3. 可合并或其他特殊状态仓位。

核心字段：

- `open_positions`
- `open_initial_value_usd`
- `open_current_value_usd`
- `unrealized_pnl_usd`
- `open_profit_positions`
- `open_loss_positions`
- `open_loss_usd`
- `open_loss_ratio`：`open_loss_usd / open_initial_value_usd`。
- `open_loss_position_ratio`：浮亏持仓数 ÷ 普通未结仓位数。
- `largest_open_position_usd`
- `largest_open_loss_usd`
- `open_position_concentration`：最大仓位初始价值 ÷ 全部未结仓位初始价值。
- `redeemable_positions`
- `redeemable_value_usd`
- `redeemable_pnl_usd`

为了避免“盈利才关闭、亏损一直持有”的假高胜率，报告必须同时显示：

- `settled_position_win_rate`：只看已结算仓位，属于最终历史指标。
- `marked_position_win_rate`：盈利结算仓位与当前浮盈仓位之和，除以结算仓位与当前普通持仓总数。
- `combined_pnl_usd`：14 天 `realized_pnl_usd + unrealized_pnl_usd`。可赎回仓位可能包含窗口外历史，因此单列 `redeemable_pnl_usd`，不混入 14 天综合 PnL。
- `hidden_loss_ratio`：`open_loss_usd / gross_profit_usd`。

`marked_position_win_rate` 必须标注为“按当前价格估值，非最终胜率”，不能覆盖或替代已结算胜率。

### 9.5 长期持亏

当前仓位接口没有可靠开仓时间时，使用本地成交缓存中同一 `condition_id + asset` 最早的未被后续卖出完全抵消的 BUY 时间近似建仓时间。

报告提供：

- `losing_positions_older_than_3d`
- `losing_positions_older_than_7d`
- `stale_losing_value_usd`
- `stale_losing_pnl_usd`

无法推导持仓年龄时设置 `position_age_unknown`，不得默认当作新仓或旧仓。

### 9.6 主领域与其他领域分层

每位员工必须保存一个明确的 `primary_domain`。报告不能只给出全钱包胜率，因为员工可能只在主领域有优势，却在其他领域持续亏损；反过来也可能出现排行榜盈利主要来自非招聘领域的情况。

每个 1/7/14 天窗口都要输出以下层级：

1. `wallet_total`：钱包全部市场。
2. `primary_domain`：员工被录用时指定的主领域。
3. `other_domains_total`：能够识别、但不属于主领域的市场合计。
4. `other_domains`：按具体领域分别统计。
5. `unknown_or_ambiguous`：无法可靠分类或同时命中多个领域的市场，单独展示，不得偷偷并入主领域。

每一层使用同一套指标：

- `gross_trade_notional_usd`
- `fill_count`
- `action_count`
- `unique_markets`
- `settled_markets`
- `realized_pnl_usd`
- `invested_usd`
- `realized_roi`
- `settled_position_win_rate`
- `settled_market_win_rate`
- `profit_factor`
- `expectancy_per_settled_market_usd`
- `open_positions`
- `unrealized_pnl_usd`
- `open_loss_usd`
- `marked_position_win_rate`
- `combined_pnl_usd`

报告还要直接给出主领域与其他领域的比较值：

- `primary_trade_notional_share`
- `primary_action_share`
- `primary_realized_profit_share`
- `primary_combined_profit_share`
- `primary_vs_other_settled_win_rate_gap`
- `primary_vs_other_marked_win_rate_gap`
- `primary_vs_other_roi_gap`
- `primary_vs_other_expectancy_gap_usd`

领域分类必须由一个公共、带版本号的分类器完成，招聘、员工统计和监控共用同一口径。修改领域关键词或分类优先级时增加 `domain_classifier_version`，然后使用本地缓存重算，不需要刷新 API。

主领域样本不足时只展示数字并标记 `primary_domain_sample_too_small`，不得因为少量幸运仓位就下结论。建议至少 8 个已结算市场才生成主领域与其他领域的强弱判断。

### 9.7 跟单价值

- 领域成交占比和领域盈利占比。
- 入场价格分布。
- BUY 成交中 `>= 0.80`、`>= 0.95` 的成交额和次数占比。
- Top 5 盈利集中度。
- 买卖双向程度。
- 同市场重复成交比例。
- 平均持仓时间。
- 24 小时内快速退出比例。
- 疑似做市/套利标记。
- 高频超短市场占比，CRYPTO 继续沿用现有 5/10/15 分钟排除规则。
- 交易金额相对员工历史 P50/P80/P95 的位置。

### 9.8 数据质量指标

- `report_status`：`complete`、`partial`、`empty`。
- `history_truncated`
- `missing_timestamp_count`
- `invalid_trade_count`
- `duplicate_trade_count`
- `position_age_unknown_count`
- `failed_components`
- `metric_schema_version`
- `rules_version`
- `domain_classifier_version`

任何核心历史分页被截断时，不允许给出“完整 14 天”结论。

## 10. 规则结论缓存

结论由程序根据固定规则生成，不由 AI 每次重新阅读原始数据后自由发挥。示例：

```json
{
  "summary_level": "caution",
  "flags": [
    "open_losses_high",
    "settled_win_rate_inflated_by_open_losses",
    "profit_concentrated"
  ],
  "facts": [
    "已结算胜率 68.0%，含当前持仓估值胜率 49.2%",
    "当前浮亏 $1,240.50，占当前投入 23.1%",
    "前 5 个盈利仓位贡献总正盈利的 79.4%"
  ]
}
```

第一版建议规则：

- `settled_win_rate_inflated_by_open_losses`：已结算胜率与估值胜率差超过 15 个百分点，且浮亏仓位不少于 3 个。
- `open_losses_high`：浮亏金额比例超过 20%。
- `stale_losses_high`：7 天以上浮亏仓位不少于 3 个，或金额超过当前投入的 15%。
- `profit_concentrated`：Top 5 正盈利贡献超过 75%。
- `high_price_dependency`：高价入场盈利依赖超过现有阈值。
- `suspected_market_making`
- `sample_too_small`
- `primary_domain_sample_too_small`
- `primary_domain_edge_confirmed`：主领域样本充足，且主领域胜率、ROI 或单市场期望显著优于其他领域。
- `outside_domain_losses_high`：其他领域综合亏损明显，应限制只跟主领域。
- `profit_not_from_primary_domain`：主领域盈利占比较低，员工的招聘领域标签可能不准确。
- `history_incomplete`

所有阈值集中配置并带版本号，修改规则后可用本地原始数据批量重算，不需要重新请求 API。

## 11. 数据新鲜度

每份报告必须同时包含以下时间，不允许只显示“报告生成时间”：

- `report_generated_at`
- `latest_trade_at`
- `latest_closed_position_at`
- `positions_observed_at`
- `activity_last_success_at`
- `trades_last_success_at`
- `closed_positions_last_success_at`
- `positions_last_success_at`
- `data_complete_from`
- `data_complete_through`

终端和 Markdown 报告固定在开头显示：

```text
报告生成：2026-06-20 14:30:00 CST（2026-06-20T06:30:00Z）
最新成交：2026-06-20 13:58:12 CST，距报告 31 分 48 秒
最新结算：2026-06-20 12:41:03 CST
当前仓位检查：2026-06-20 14:29:44 CST，距报告 16 秒
14 天历史完整性：完整 / 截断 / 部分失败
```

新鲜度只负责提示，不触发刷新。建议提供可配置的展示警告阈值，默认 6 小时：超过阈值显示 `STALE`，但仍返回缓存数据。

## 12. CLI 设计

建议在现有 Rust 二进制中增加 `employee-stats` 子命令，而不是创建另一套网络脚本。

从用户视角，它就是一个固定的“候选员工深查脚本”。用户只需要提供钱包地址，不需要了解底层 API、分页或指标计算。

约定的自然语言工作流：

```text
用户：深查这个候选员工 0xabc...
Codex：执行一次 employee-stats refresh，读取紧凑报告并给出结论。

用户：再看看 0xabc... 的表现
Codex：执行 employee-stats show，只读缓存，不访问 API。

用户：刷新 0xabc... 的数据
Codex：再次执行 employee-stats refresh，然后说明新的数据截至时间。

用户：只刷新 0xabc... 的持仓
Codex：执行 employee-stats refresh --only positions。
```

第一次明确要求深查某个候选员工，就视为允许为该地址执行一次初始刷新。之后除非用户明确说“刷新”“更新”或指定刷新组件，否则一律读取缓存。

### 12.1 读取缓存

```bash
smart-wallet-discovery employee-stats show \
  --wallet 0xabc...
```

也支持已缓存的唯一别名：

```bash
smart-wallet-discovery employee-stats show \
  --employee WeatherHK
```

`show` 必须保证零 API 请求。

### 12.2 首次建立或完整刷新

```bash
smart-wallet-discovery employee-stats refresh \
  --employee '0xabc...:name:WEATHER:weather|temperature' \
  --window-days 14
```

### 12.3 单独刷新组件

```bash
smart-wallet-discovery employee-stats refresh \
  --wallet 0xabc... \
  --only positions
```

可选组件：

```text
activity,trades,closed-positions,positions
```

多个组件使用逗号分隔。

### 12.4 用缓存重算

```bash
smart-wallet-discovery employee-stats rebuild \
  --wallet 0xabc...
```

`rebuild` 只用本地原始数据，适用于指标公式或规则版本升级。

### 12.5 输出格式

```bash
--format text
--format json
--format markdown
--format compact-json
```

默认 `text`。无论格式为何，报告都必须包含数据时间和完整性字段。

给 Codex 使用时默认选择 `compact-json`：标准输出只包含员工身份、数据时间、核心 14 天指标、主领域对比、专项拆分、持仓风险、数据质量和规则结论，目标大小控制在数 KB 内。完整报告和原始数据只写入 SQLite、`latest.json` 和 `latest.md`，不得把数百条原始成交输出到对话上下文。

这条约束是减少 token 消耗的核心：API 数据由程序在本地消化，Codex 读取固定结构的最终统计，不再逐条阅读和重复推导。需要新增领域或主题口径时，优先沉淀为脚本规则并通过 `rebuild` 离线重算，而不是在对话里手工筛原始成交。

## 13. 报告结构

JSON 顶层建议结构：

```json
{
  "schema_version": 1,
  "metric_schema_version": 1,
  "rules_version": 1,
  "employee": {},
  "freshness": {},
  "data_quality": {},
  "windows": {
    "1d": {
      "wallet_total": {},
      "primary_domain": {},
      "other_domains_total": {},
      "other_domains": {},
      "unknown_or_ambiguous": {},
      "specialties": {
        "WORLD_CUP": {}
      }
    },
    "7d": {},
    "14d": {}
  },
  "current_positions": {},
  "copyability": {},
  "conclusion": {}
}
```

Markdown 展示顺序：

1. 员工身份与数据截至时间。
2. 14 天核心结论。
3. 已结算胜率与含持仓估值胜率对比。
4. 成交行为。
5. 当前持仓与长期持亏。
6. 主领域、其他领域和专项主题的胜率、ROI、PnL 和当前持仓对比。
7. 盈利质量与集中度。
8. 跟单价值。
9. 数据质量和警告。

## 14. 与员工发现流程的集成

推荐流程：

```text
发现候选人
  -> 现有招聘健康检查
  -> 人工或规则确认正式录用
  -> 初次 employee-stats refresh
  -> 保存员工档案和 14 天基线
  -> watch 启动时只读取缓存画像
  -> 用户要求时单独 refresh
```

不要给招聘扫描遇到的每个候选钱包建立完整档案，否则会放大 API 消耗。只有正式录用或明确要求深查的候选人才进入缓存系统。

`watch` 启动时不得因为员工统计过旧而阻塞，也不得隐式刷新。它可以显示档案年龄，并继续使用最近一次成功报告。

## 15. 实施阶段

### 阶段 A：统一指标核心

- 新建员工统计领域模型。
- 从 `profile.rs` 和 `leaderboard_recruitment.rs` 抽取公共函数。
- 完成 1/7/14 天、已结算、当前持仓和估值胜率指标。
- 保持现有招聘和监控行为不变。

交付条件：同一输入在招聘报告和员工统计报告中产生相同 PnL、ROI、集中度和当前浮亏结果。

### 阶段 B：SQLite 缓存与同步

- 增加数据库迁移和数据访问层。
- 实现 `/activity` 与 `/trades` 合并去重。
- 实现按时间边界分页。
- 实现增量刷新和组件状态。
- 实现历史截断检测。

交付条件：同一员工连续刷新两次不会产生重复成交；第二次只处理重叠窗口和新数据。

### 阶段 C：CLI 与报告

- 实现 `show`、`refresh`、`rebuild`。
- 实现 `--only`。
- 输出 text、JSON、Markdown。
- 强制显示数据截至时间和完整性。

交付条件：`show` 在断网环境仍能完整返回最近缓存报告。

### 阶段 D：发现和监控集成

- 正式录用员工后触发一次初始档案建立。
- `watch` 加载缓存画像，不隐式刷新。
- 保留现有 `profiles` 命令兼容入口，内部改用统一统计核心；之后再决定是否废弃旧入口。

### 阶段 E：稳定性和运维

- 增加数据库备份或损坏恢复说明。
- 增加旧快照压缩策略。
- 增加规则版本重算。
- 更新 README 和示例命令。

## 16. 测试计划

### 16.1 单元测试

- 同一交易同时出现在 `/activity` 和 `/trades` 时只计算一次。
- 同一交易哈希包含多笔成交时不会错误合并。
- 120 秒内碎片成交正确合并为一次出手。
- 超过 120 秒的成交分为两次出手。
- 秒和毫秒时间戳均正确标准化。
- 盈利、亏损、持平仓位的胜率分母正确。
- 一个市场持有多个结果仓位时，市场胜率正确合并。
- 主领域、其他领域和无法分类市场的成交与仓位不会重复计数。
- 全钱包指标等于主领域、其他领域和无法分类部分之和。
- 主领域已结算胜率、估值胜率、ROI 和综合 PnL 分别正确计算。
- 领域分类规则变化后可通过本地 `rebuild` 重算。
- 浮亏持仓能降低 `marked_position_win_rate`。
- 可赎回仓位不会混入普通未结仓位。
- 当前浮亏会进入 `combined_pnl_usd`。
- 没有亏损时 `profit_factor` 返回 `null` 和解释。
- 历史截断时报告状态不是 `complete`。
- 无法推导建仓时间时设置未知标记。

### 16.2 集成测试

- 使用固定 API 样本完成首次同步、第二次增量刷新和重算。
- 断网执行 `show`，确认没有网络调用。
- 只刷新 `positions`，确认其他组件成功时间不变。
- 模拟 `/closed-positions` 失败，确认旧数据保留且报告为 `partial`。
- 模拟高频钱包超过 100 条成交，确认分页直到 14 天边界。
- 同一别名对应多个钱包时返回歧义错误。

### 16.3 回归测试

- 现有 `scan-domain-employees` 指标不变。
- 现有 `profiles` 输出的核心字段保持兼容。
- `watch` 不增加隐式 API 请求。
- WeatherHK 自动跟单状态和执行路径不受影响。

## 17. 验收标准

只有同时满足以下条件，第一版才算完成：

1. 输入钱包地址可以生成最近 14 天员工报告。
2. 报告包含成交额、成交笔数、出手次数、金额均值和分位数。
3. 报告同时包含已结算胜率、含持仓估值胜率、已实现 PnL、未实现 PnL 和综合 PnL。
4. 报告分别展示全钱包、员工主领域、其他领域合计及各个其他领域的胜率、ROI、PnL 和持仓风险。
5. 能识别员工长期持有浮亏仓位导致的胜率偏差。
6. 能识别盈利是否真正来自员工主领域，以及其他领域是否拖累表现。
7. 高频员工不会因固定前 100 条或固定页数造成静默漏数。
8. 报告明确显示最新成交、最新结算、当前仓位检查时间和历史完整性。
9. `show` 不调用 API。
10. `refresh` 可以只刷新一个员工。
11. `refresh --only positions` 可以只刷新当前仓位。
12. 指标、规则和领域分类可以只用本地数据重算。
13. 招聘扫描不会自动为所有候选人建立昂贵档案。
14. `compact-json` 不输出原始成交，只返回足够形成结论的紧凑统计。
15. 同一员工在没有明确刷新指令时，重复询问不会产生 API 请求。
16. Surf 不作为本模块依赖。
17. `fifwc-*`、`world-cup` 和 `World Cup` 市场会归入 `SPORTS`，并单独输出 `specialties.WORLD_CUP` 指标。
18. 当已结算二元仓位的 `realizedPnl` 近似为 0 但 `curPrice` 已变为 1 或 0 时，指标用结算价重算胜率和 PnL，避免隐藏赢亏。

## 18. 第二阶段候选能力

- 保存员工成交后的市场价格轨迹。
- 计算 30 秒、2 分钟、5 分钟延迟跟单收益。
- 引入盘口深度、可成交金额和滑点。
- 对员工统计做 14 天对前 14 天的趋势比较。
- 记录员工晋升、降级和暂停时间线。
- 生成多个员工横向比较报告。
- 在不自动刷新数据的前提下，为过旧档案提供统一待刷新清单。

这些能力不能改变第一版的原则：地址为主键、本地缓存优先、数据时间透明、刷新由用户明确触发。
