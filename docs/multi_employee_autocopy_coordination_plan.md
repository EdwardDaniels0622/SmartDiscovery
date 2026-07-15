# 多员工自动跟单全局协调与 Outcome 手动接管实施文档

## 1. 背景

当前自动跟单从单员工 WeatherHK 扩展到多员工后，暴露出两个结构性问题：

1. 多个员工可能在同一个天气领域、同一个市场、同一个选项上同时产生信号，但系统仍按每个员工独立 state 决策。
2. 用户手动取消某个 outcome 的挂单，本意是接管这个仓位选项，但系统会把取消理解为“承诺仓位不足”，随后继续补挂，甚至触发卖出和再买入循环。

根因是：策略 state 按员工拆开，但真实 CLOB 钱包仓位是全局共享的。同一个 token 余额无法天然区分属于 WeatherHK、OlympusHive，还是未来其他员工贡献的信号。

本文档设计两个改造：

- 全局 outcome 协调层：多员工信号先汇总成全局目标，再统一决定真实钱包买卖。
- Exact outcome 手动接管：用户可以对某个 `condition_id + asset` 精准暂停自动动作，不影响其他温度档、员工或市场。

## 2. 目标

- 同一个 exact outcome 的全局风险上限固定为 `50U`，包含已成交仓位和未成交 BUY 挂单。
- 多员工同时支持同一 outcome 时，不重复加仓突破上限。
- 某个员工卖出或源仓位对账显示不持有时，只更新该员工贡献，不直接卖掉整个真实钱包仓位。
- 用户手动取消某个 outcome 的挂单后，该 exact outcome 自动进入手动接管，系统不再自动买、卖、补挂或退出重试。
- 源仓位对账降级为“员工目标更新来源”，不再绕过全局协调直接下单。
- Telegram 提示能解释：谁触发、其他员工是否仍支持、全局目标如何变化、为什么买/卖/跳过。

## 3. 非目标

- 初版不做收益最大化或复杂员工权重优化。
- 初版不按员工精确拆分真实 token 批次成本；先用美元目标和当前全局仓位近似。
- 初版不尝试自动判断用户手动取消后的最佳卖出动作；手动接管后只停止自动动作。
- 初版不做前端 UI；先通过 state 文件、日志和 Telegram 提示完成闭环。

## 4. 核心概念

### 4.1 Exact Outcome Key

所有暂停、归属、上限、挂单协调都绑定 exact outcome：

```text
position_key = condition_id + ":" + asset
```

它代表某个市场的某个具体选项，例如：

```text
Will the highest temperature in Shenzhen be 29°C on July 6? / Yes
```

该 key 不影响同市场其他温度档或其他方向。

### 4.2 员工贡献目标

每个员工对某个 outcome 只贡献一个目标金额：

```text
employee_target_usd = f(source_activity, source_position, rules)
```

例如 OlympusHive 在某 outcome 累计买入 `80U`，按 50% 跟单，则贡献目标是 `40U`。

员工卖出、源仓位归零、短线卖压保护，都只改变该员工自己的贡献目标。

### 4.3 全局目标

同一 outcome 的真实钱包目标由所有员工贡献汇总得到：

```text
global_target_usd = min(sum(employee_target_usd), global_outcome_cap_usd)
```

默认：

```text
global_outcome_cap_usd = 50U
```

### 4.4 已承诺仓位

全局已承诺仓位包含：

```text
committed_usd = filled_position_cost_usd + open_buy_order_remaining_usd
```

因此未成交 BUY 挂单也占用 50U 上限。

## 5. 需求一：多员工全局 Outcome 协调

### 5.1 买入原则

当任意员工 BUY 或源仓位对账补目标时：

1. 更新该员工在 `position_key` 上的 `employee_target_usd`。
2. 汇总所有员工贡献，计算 `global_target_usd`。
3. 读取全局已承诺仓位 `committed_usd`。
4. 如果 `committed_usd >= global_target_usd`，不加仓。
5. 如果存在缺口，只能在剩余额度内买入或挂单：

```text
buy_gap_usd = global_target_usd - committed_usd
```

同一 outcome 初版只允许一个全局 pending BUY。新信号到来时：

- 如果已有挂单价格仍合理，保持原挂单。
- 如果新信号价格更高且来自明确 BUY，可以取消旧挂单并重挂剩余额度。
- 如果新信息只来自源仓位对账，不允许抬价重挂。
- 如果 outcome 已手动接管，不做任何自动买入。

### 5.2 卖出原则

当员工 SELL 到来：

1. 只降低该员工自己的 `employee_target_usd` 或按比例减少。
2. 重新计算 `global_target_usd`。
3. 如果其他员工仍支持该 outcome，保留对应目标。
4. 仅当真实仓位或已承诺仓位超过新的全局目标时，卖出超额部分。

示例：

```text
WeatherHK target = 50U
OlympusHive target = 50U
global_target = 50U
```

WeatherHK 卖出后：

```text
WeatherHK target = 0U
OlympusHive target = 50U
global_target = 50U
```

结果：不卖，因为全局目标没变。

另一个示例：

```text
WeatherHK target = 25U
OlympusHive target = 25U
global_target = 50U
```

WeatherHK 清仓后：

```text
WeatherHK target = 0U
OlympusHive target = 25U
global_target = 25U
```

如果我方真实仓位约 `50U`，只卖出超额 `25U`，不全清。

### 5.3 源仓位对账原则

源仓位对账从“直接下单者”降级为“目标更新来源”。

允许：

- 员工仍持仓，更新该员工目标。
- 员工不持仓，将该员工目标降为 0。
- 员工持仓增加，但没有明确 BUY 时，作为低优先级补漏信号。

禁止：

- 源仓位对账直接发起真实 SELL。
- 源仓位对账绕过全局 cap 发起 BUY。
- 源仓位对账用旧均价抬高已有挂单价格。
- 源仓位对账在 outcome 手动接管期间做任何交易动作。

### 5.4 价格仲裁

价格信号优先级：

1. 明确实时 BUY/SELL 成交价。
2. 新鲜 `/positions` 持仓均价或 current price。
3. 旧 source metadata。

初版规则：

- 明确 BUY 可以触发追价或重挂，但必须受全局剩余额度限制。
- 源仓位对账只能用来补低价挂单，不允许抬价追买。
- 如果已有挂单价格低于新价格，默认不因为源仓位对账取消重挂。
- 如果价格已经远离旧挂单，且新信号只是 positions，对账只发提醒，不下单。

### 5.5 单 Outcome Pending BUY 规则

每个 `position_key` 初版最多一个全局 pending BUY。

需要记录：

- `position_key`
- `order_id`
- `limit_price`
- `remaining_usd`
- `trigger_employee`
- `supporting_employees`
- `created_reason`
- `source_trade_at_secs`

多个员工信号不能各自挂一张独立 BUY，从而突破 50U。

## 6. 需求二：Exact Outcome 手动接管

### 6.1 触发方式

手动接管绑定 `condition_id + asset`，不是全局暂停。

触发来源：

- 检测到用户手动取消该 outcome 的 pending BUY。
- 后续可增加 Telegram 命令或 CLI 命令显式暂停。

系统主动取消不应触发手动接管，例如：

- 员工 SELL 触发撤单。
- TTL 到期。
- 系统重挂前主动撤单。
- 源仓位对账判断不适合继续挂单。

需要在 state 中区分“系统发起取消”和“外部取消”。如果同步订单时发现订单已取消，且本地没有对应系统取消意图，则视为用户手动取消。

### 6.2 暂停范围

手动接管后，仅暂停该 exact outcome：

- 不自动 BUY。
- 不自动 SELL。
- 不源仓位对账补挂。
- 不源仓位对账清仓。
- 不退出重试。
- 不因其他员工仍持有而重新补挂。
- 只发送提醒，不下单。

不影响：

- 同市场其他温度档。
- 同城市其他日期。
- 其他员工的其他 outcome。
- 全局 watcher 或其他自动跟单服务。

### 6.3 暂停期限

默认永久暂停，直到手动恢复或市场结束。

原因：旧低价机会错过后，后续再次出现低价可能已经是市场信息突变，不应自动接单。

### 6.4 手动接管状态结构

建议在全局 state 中新增：

```json
{
  "manual_outcome_pauses": [
    {
      "position_key": "condition:asset",
      "market_title": "...",
      "outcome": "Yes",
      "mode": "manual-takeover",
      "reason": "detected external cancel of pending BUY",
      "created_at_secs": 123,
      "created_by": "external-cancel",
      "expires_at_secs": 0,
      "last_notified_at_secs": 123
    }
  ]
}
```

`expires_at_secs = 0` 表示永久，直到显式恢复。

### 6.5 Telegram 提示

检测到手动接管：

```text
[Outcome 手动接管]
市场: Will the highest temperature in Shenzhen be 29°C on July 6?
方向: Yes
原因: 检测到用户手动取消挂单
动作: 已暂停该 outcome 的自动买入、自动卖出、源仓位对账、退出重试
影响范围: 仅此 condition_id + asset，不影响其他温度档/员工/市场
```

暂停期间遇到员工 BUY/SELL：

```text
[Outcome 已手动接管 / 只提醒不交易]
员工: OlympusHive
动作: BUY 12.69U @ 6.00c
市场: ...
方向: Yes
处理: 不自动买入/卖出，因为该 exact outcome 已手动接管
```

需要做通知限频，避免同 outcome 高频 BUY 时刷屏。

## 7. 新全局 State 设计

建议新增全局协调 state 文件：

```text
logs/autocopy_global_state.json
```

核心结构：

```json
{
  "schema_version": 1,
  "outcomes": [
    {
      "position_key": "condition:asset",
      "market_title": "...",
      "outcome": "Yes",
      "asset": "...",
      "condition_id": "...",
      "global_cap_usd": 50.0,
      "global_target_usd": 42.5,
      "committed_usd": 40.0,
      "employee_targets": [
        {
          "source_name": "WeatherHK",
          "source_wallet": "0x...",
          "target_usd": 0.0,
          "last_signal": "SELL",
          "last_signal_at_secs": 123,
          "target_source": "activity-sell"
        },
        {
          "source_name": "OlympusHive",
          "source_wallet": "0x...",
          "target_usd": 42.5,
          "last_signal": "BUY",
          "last_signal_at_secs": 456,
          "target_source": "activity-buy"
        }
      ],
      "pending_buy": {
        "order_id": "0x...",
        "remaining_usd": 7.5,
        "limit_price": 0.1993,
        "trigger_employee": "OlympusHive"
      },
      "manual_pause": null
    }
  ]
}
```

初版可以不立即迁移全部历史 state，但所有新动作必须经过全局 state 判断。

### 7.1 阶段 1 已落地状态（2026-07-08）

已在代码中增加共享协调 state，默认路径：

```text
logs/autocopy_global_state.json
```

可通过 `.env` 覆盖：

```text
WEATHERHK_GLOBAL_STATE_PATH=logs/autocopy_global_state.json
```

当前实现采用“员工 + exact outcome”的快照结构：

- 每个员工实例发布自己的 `target_usd`、已成交成本、未成交 BUY 挂单、短暂下单预留。
- 任意 BUY 下单前先汇总同 `position_key` 的多员工目标，再按 `copy_target_cap_usd`（默认 50U）裁剪。
- 已成交仓位、未成交 BUY、下单前短暂 reservation 都会占用全局额度。
- 同一个 exact outcome 当前只允许一个活跃全局 BUY pending；已有 pending 时新信号不叠第二张挂单。
- 实时 BUY 和源仓位对账补挂都必须经过全局 cap。
- pending 同步、取消、卖出后会刷新全局 committed，避免额度长期偏旧。
- 活跃服务会定期刷新本地持仓/挂单快照；超过活跃窗口的旧快照不会继续卡住全局额度。

尚未在阶段 1 改动的内容：

- SELL 员工贡献卖出协调已在阶段 2 实现。
- 手动接管、外部取消识别属于阶段 0/后续实现范围，本次未启用。
- 本次只改仓库代码和测试，未部署、未重启任何跟单服务。

### 7.2 阶段 2 已落地状态（2026-07-08）

已在代码中增加员工贡献 target 的卖出协调：

- 员工 SELL 到来时，先降低该员工在 exact outcome 上的 `target_usd`。
- 重新汇总同 outcome 多员工目标，计算新的全局目标。
- 如果其他员工仍支持，且全局 committed 没有超过新目标，则只提示/跳过，不卖真实钱包仓位。
- 如果全局 committed 超过新目标，只按超额比例卖出，不再因为某一个员工退出就清空整个 outcome。
- 源仓位对账显示某员工不持有 outcome 时，也只把该员工 target 降为 0，再通过全局超额判断是否卖出。
- 本地 state 或全局 state 尚未记录 committed、但 SELL 信号能推断出员工旧 target 时，会用 SELL 金额和减仓比例作为 fallback，保留“先查真实钱包余额再按比例卖”的恢复能力。
- Telegram 原因文案会展示员工目标变化、全局目标变化、全局 committed、超额金额、卖出比例，以及其他仍支持的员工。

仍未覆盖：

- 手动接管/外部取消识别仍未实现。
- 不做员工成本批次级别拆分；阶段 2 仍使用美元 target 和全局比例来近似。
- 本次只改仓库代码和测试，未部署、未重启任何跟单服务。

### 7.3 阶段 3 已落地状态（2026-07-08）

已在代码中完成价格仲裁与源仓位对账降权：

- 明确 BUY activity 仍是最高优先级价格信号；实时 BUY 可以触发旧挂单撤单/重挂。
- `/positions` 生成的 synthetic metadata 不再写入 `last_buy_price`，避免把持仓均价误当成明确买入价。
- 源仓位对账补漏如果没有明确 BUY 价格，只按员工持仓均价 post-only，不再额外加 5% 或 2c 溢价。
- 如果已有 BUY pending，而 `/positions` 或对账均价暗示应该抬价，系统不再取消旧单重挂；只发“对账不抬价重挂”提示，并带冷却避免刷屏。
- 已有 pending BUY 是否取消，仍只由缺源仓、价格/风控不合格、目标超额、TTL 或明确 BUY 重挂逻辑决定。
- 高价风控、全局 cap、单 pending BUY、员工贡献 SELL 协调继续生效。

仍未覆盖：

- 手动接管/外部取消识别仍未实现。
- 对账不抬价提示目前是本地冷却提示，不是完整的 Telegram 命令式恢复/确认流程。
- 本次只改仓库代码和测试，未部署、未重启任何跟单服务。

## 8. 执行流程设计

### 8.1 BUY 流程

```text
收到员工 BUY
  -> 如果 outcome 手动接管：只提醒，不交易
  -> 更新 employee_target
  -> 计算 global_target = min(sum(employee targets), 50U)
  -> 读取 committed = 已成交 + pending BUY
  -> 如果 committed >= global_target：不买
  -> 如果已有 pending BUY：
       -> 判断是否需要保留/取消重挂
  -> 如果无 pending BUY：
       -> 按 buy_gap_usd 下单或挂单
```

### 8.2 SELL 流程

```text
收到员工 SELL
  -> 如果 outcome 手动接管：只提醒，不交易
  -> 降低该员工 employee_target
  -> 计算 new_global_target
  -> 如果真实/承诺仓位 <= new_global_target：不卖
  -> 如果超额：
       -> 取消超额 pending BUY
       -> 必要时卖出超额真实仓位
```

### 8.3 源仓位对账流程

```text
周期性读取员工 /positions
  -> 对每个员工更新 employee_target
  -> 不直接下单
  -> 重新跑 global reconcile
  -> 若 outcome 手动接管：跳过交易动作
```

### 8.4 手动取消检测流程

```text
同步 pending BUY
  -> 若状态为 cancelled
  -> 若本地没有 system_cancel_intent
  -> 标记 manual_pause(position_key)
  -> 移除 pending BUY
  -> 发送 Outcome 手动接管提示
```

## 9. 分阶段实施计划

### 阶段 0：立即止血

目标：防止继续出现“取消后又补挂”与“源仓位对账误卖”。

改动：

- 增加 `manual_outcome_pauses`。
- 外部取消 pending BUY 自动加入手动接管。
- 手动接管期间拦截 BUY、SELL、源仓位对账补挂、源仓位对账清仓、退出重试。
- 源仓位对账清仓在多员工模式下先改为只提醒不交易。

风险：可能少卖一些该卖的仓位，但不会再误卖或反复补挂。

### 阶段 1：全局 cap 与单 pending BUY

目标：同 outcome 多员工不会突破 50U。

状态：已在仓库实现并通过 `cargo test autocopy -- --nocapture`，尚未部署或重启线上跟单服务。

改动：

- 新增全局 outcome state。
- BUY 前读取全局已成交和 pending BUY。
- 每个 outcome 最多一个 pending BUY。
- 多员工补仓统一计算剩余额度。
- 源仓位对账补挂必须通过全局 cap。

风险：员工目标归属仍是粗略美元目标，但足以防止结构性超仓。

### 阶段 2：员工贡献目标与卖出协调

目标：A 员工卖出不会误伤 B 员工仍支持的仓位。

状态：已在仓库实现并通过 `cargo test autocopy -- --nocapture`，尚未部署或重启线上跟单服务。

改动：

- 每个 outcome 记录各员工 `employee_target_usd`。
- SELL 只降低该员工目标。
- 真实卖出只卖全局目标下降后的超额部分。
- Telegram 展示其他员工是否仍支持。

风险：无法精确匹配每个员工对应的成本批次，但自动动作方向会正确。

### 阶段 3：价格仲裁与对账降权

目标：减少旧 positions 价格导致的追补和过时挂单。

状态：已在仓库实现并通过 `cargo test autocopy -- --nocapture`，尚未部署或重启线上跟单服务。

改动：

- BUY activity 价格优先于 positions。
- positions 只能补漏，不能抬价重挂。
- 挂单价格明显过时只提醒，等待新 BUY。
- 用户手动接管后必须显式恢复。

## 10. 测试清单

### 10.1 手动接管

- 用户手动取消 pending BUY 后，该 outcome 进入 manual takeover。
- 手动接管只影响相同 `condition_id + asset`。
- 手动接管期间员工 BUY 不下单。
- 手动接管期间员工 SELL 不下单。
- 手动接管期间源仓位对账不补挂、不清仓。
- 系统主动取消订单不触发手动接管。

### 10.2 多员工同 Outcome

- WeatherHK 和 OlympusHive 同时买同 outcome，总目标不超过 50U。
- 已成交 30U、pending 15U 时，后续最多只能补 5U。
- 一个员工卖出但另一个员工仍目标 50U，不卖。
- 一个员工卖出后全局目标从 50U 降到 25U，只卖超额 25U。
- 源仓位对账显示某员工不持有，只降低该员工目标，不直接清仓。

### 10.3 价格与挂单

- 同 outcome 不能出现多个全局 pending BUY。
- 源仓位对账不能用旧均价抬价重挂。
- 明确 BUY 可以触发重挂，但仍受剩余额度限制。
- pending BUY 成交后正确更新 committed。
- pending BUY 外部取消后正确进入手动接管。

## 11. 部署与迁移

上线前：

- 备份现有 `weatherhk_autocopy_state.json` 和 `weatherf421_autocopy_state.json`。
- 默认启用阶段 0 的手动接管保护。
- 默认在多员工模式下关闭源仓位对账清仓实盘动作。

上线后：

- 先观察 24 小时 Telegram 提示是否准确。
- 确认没有重复补挂和误卖后，再启用阶段 1 的全局 pending BUY。
- 阶段 2 前不建议新增更多自动跟单员工。

## 12. 待确认问题

- 手动接管恢复方式先用本地配置/state 手动删除，还是加 Telegram 命令？
- 手动接管是否需要分“只停买入”和“完全接管”两种模式？当前建议初版只做完全接管。
- 全局 cap 是固定 50U，还是后续支持按 market/outcome/员工质量调整？
- 源仓位对账在员工低频但稳定时是否允许补漏？当前建议只更新目标，不直接交易。

## 13. 推荐初版决策

优先实施阶段 0 和阶段 1：

1. Exact outcome 手动接管，最高优先级拦截所有自动交易动作。
2. 源仓位对账清仓多员工模式下只提醒不交易。
3. 同 outcome 全局 50U cap，已成交 + pending BUY 一起计算。
4. 同 outcome 只允许一个全局 pending BUY。

这四条可以先防住最严重损失：误卖、取消后补挂、跨员工串仓、重复挂单超额。后续再逐步优化员工贡献和卖出比例。
