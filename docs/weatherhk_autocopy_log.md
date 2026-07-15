# WeatherHK 自动跟随记录

本文档记录 WeatherHK 小额自动跟随的需求、实现取舍和已修复问题，方便后续回滚、复盘和版本管理。

## 当前目标

- 只对 `WeatherHK` 钱包启用自动跟随。
- 用小额资金复制其 WEATHER 市场买卖节奏，验证能否跟到短线优势。
- 保持单笔、单市场、每日投入和每日亏损上限。
- 私钥和 funder 地址只放本地 `.env`，仓库只提交 `.env.example`。

## 当前核心规则

- 实时数据源使用 Polymarket Data API `/activity`，不再用 `/trades` 做实时 watcher。
- WeatherHK 常驻 watcher 使用 `--trade-limit 100`，员工级扫描间隔为 `1s`。
- `/activity` 连续失败时自动退避：第一次失败后至少 `3s`，第二次及以后至少 `5s`；下一次成功后恢复员工配置的 `1s`。
- 外部 executor 对 CLOB 网络型错误自动重试：默认 `POLYMARKET_EXECUTOR_RETRIES=2`，退避基准 `POLYMARKET_EXECUTOR_RETRY_BACKOFF_SECONDS=0.4`；不重试地区限制、余额/授权不足、最小订单量不足、无可撮合流动性等确定失败。
- 源 BUY 金额不再因 `<1U` 默认跳过：`WEATHERHK_MIN_BUY_SOURCE_NOTIONAL_USD=0`。
- 高价低收益 BUY 默认跳过：`> 98c`。
- 全员仓位限制：任意员工、任意价格区间，按比例计算出的同 exact outcome 目标最多 `50U`；已有超过 50U 的仓位不主动调仓，后续只停止继续加仓。
- 高价仓位限制：源成交价或我方可能入场价 `>90c` 且未超过 `98c` 时，继续受 `50U` 目标上限保护，并额外限制追价。
- 高价追价限制：源成交价 `>90c` 且未超过 `98c` 时，最高默认只买到 WeatherHK 的源成交价；盘口更贵则按源价挂单，不向 `99c/100c` 追价。
- 低价 BUY 默认不再按价格跳过：`WEATHERHK_SKIP_BUY_PRICE_AT_OR_BELOW=0`。
- 跟单金额按 WeatherHK 源成交金额的 `50%`：
  - BUY 一律按源金额 `50%`，金额再小也不因本地最低跟单金额跳过。
  - 没及时吃到需要挂单时，挂单金额同样按源总额 `50%`。
- BUY 执行：
  - 如果当前卖一在直接追价上限内，允许小额 FOK 买入。
  - 如果卖一超出直接追价上限或不可用，则挂 post-only 限价单。
- SELL 执行：
  - WeatherHK SELL 先取消同 asset 的 pending BUY。
  - 任意金额 SELL 若我方有仓位，使用 FAK 市场退出，不设最低卖价/滑点保护；能成交多少先成交多少，剩余自动取消。
- pending BUY 取消条件：
  - WeatherHK 对同 asset SELL。
  - WeatherHK 当前持仓中已不包含该 asset。
  - TTL 到期，若 TTL 非 0。
  - 手动或执行器返回取消/失败。
- pending 同步通知：
  - 普通 pending sync 不发 Telegram。
  - 只在成交、取消、失败或部分成交增加时通知。

## 已修复问题

### 1. 便宜卖一被错误跳过

- 现象：WeatherHK 买入价约 `6.4c`，跟单时看到可成交价更便宜，却因为 `post-only` 保护被跳过。
- 原因：旧逻辑把 `best ask <= passive bid` 视为“会吃单”的风险，忽略了“更便宜成交其实更好”。
- 修复：打开 `WEATHERHK_BUY_TAKE_ENABLED=true`。当 `best ask <= direct_limit_price` 时允许 FOK 买入。

### 2. 小于 1U 的源 BUY 被放大成 1U 跟单

- 现象：WeatherHK 有时只买几十美分，我方仍按 `<10U => 1U` 跟单。
- 风险：碎片/试探成交被放大，风险比例失真。
- 修复：新增 `WEATHERHK_MIN_BUY_SOURCE_NOTIONAL_USD=1`，源 BUY `<1U` 跳过。

### 3. 98c/99c 高概率薄利单被跟随

- 现象：WeatherHK 有很多利润极薄的高概率成交，疑似挂单/做市型被吃。
- 风险：跟随者成本和滑点会吃掉薄利。
- 修复：默认跳过 `>98c` 的 BUY；`98.00c` 本身不跳过。

### 4. 0c/极低价格尾部单过滤已关闭

- 旧判断：接近 0 的尾部成交可能是极小概率尾部或 stale maker 噪音，因此默认跳过 `<=0.5c` 的 BUY。
- 新观察：WeatherHK 的低价尾部单可能正是优势来源之一，按价格小于 1c/0.5c 直接忽略会漏单。
- 调整：默认关闭低价价格过滤，`WEATHERHK_SKIP_BUY_PRICE_AT_OR_BELOW=0`；仍保留源 BUY 金额 `<1U`、高价薄利、短线卖压等其他保护。

### 5. pending 普通同步刷屏

- 现象：同一张挂单每次 sync 都发送 `[WeatherHK 挂单同步] 状态: pending`。
- 风险：Telegram 429 限流，用户无法分辨真实事件。
- 修复：普通 pending sync 只更新状态文件，不发通知；只报告成交、取消、失败和部分成交进展。

### 6. 小额 SELL 先被 dust 过滤，导致挂单未取消

- 现象：WeatherHK 卖出后，相关 pending BUY 仍在。
- 原因：旧顺序先判断 dust SELL，再取消挂单。
- 修复：SELL 处理顺序改为先取消 pending BUY，再判断是否同步卖出我方仓位。

### 7. `/trades` 漏掉 WeatherHK 的 31°C SELL

- 现象：用户在 Polymarket UI 看到 WeatherHK 卖出 `Will the highest temperature in Hong Kong be 31°C on June 3?`，但 watcher 使用 `/trades?user=...` 没看到这笔 SELL，pending BUY 一直挂着。
- 验证：
  - `/trades?user=WeatherHK` 没有返回该笔 `31°C SELL`。
  - `/activity?user=WeatherHK` 返回了该笔：
    - 时间：`2026-06-03 09:41:20 CST`
    - 方向：`SELL`
    - 市场：`Will the highest temperature in Hong Kong be 31°C on June 3?`
    - 价格：`2.10c`
    - 数量：`907.06`
    - 名义金额：约 `19.05U`
    - 交易哈希：`0xd2ba...fbcc6`
- 结论：不是用户看错，也不是整个 Data API 都延迟；实时 watcher 使用的 `/trades` 用户交易流不适合 WeatherHK。
- 修复：
  - 实时 watcher 切换到 `/activity`。
  - WeatherHK 启动脚本增加 `--trade-limit 100`。
  - 增加 source-position reconcile：如果 WeatherHK 当前持仓不再包含 pending BUY 的 asset，则取消该 pending BUY。

### 8. SELL 执行失败导致 Telegram 刷屏

- 现象：WeatherHK 对同一市场连续拆单卖出时，我方 SELL 同步因 CLOB 返回 `not enough balance / allowance` 失败，watcher 对每笔源 SELL 都重复发送 `[WeatherHK 自动跟随跳过] 状态: failed`。
- 原因：
  - `failed` 被错误显示成“跳过”，用户无法区分主动风控跳过和执行失败。
  - 对同一 market/outcome 的 SELL 执行失败没有冷却，后续拆单 SELL 会继续重复尝试并重复通知。
- 修复：
  - `failed` 文案改为“失败”，不再显示成“跳过”。
  - 新增 `WEATHERHK_FAILED_ACTION_COOLDOWN_SECONDS`，默认 `900` 秒；同一 action + market/outcome 在冷却期内不再重复执行失败动作。
  - 同类 failed 报告在冷却期内只发第一条，避免 Telegram 刷屏和 429 限流。

### 9. 首笔 SELL 即时风控 + 同 outcome 短线卖压保护

- 背景：WeatherHK 会在同一温度 outcome 上短时间大量 SELL，夹杂少量小额 BUY。这更像挂单卖出被吃、止盈出货或程序化库存调整；如果我方延迟跟 BUY，容易买在出货/掉价前。
- 原则：
  - 第一笔 SELL 不等待行为识别，立即取消同 `condition_id + asset` 的 pending BUY；若我方有仓位，无论 WeatherHK 本笔 SELL 金额大小，都使用市场退出模式同步卖出，不做最低卖价/滑点保护。
  - 行为识别只绑定 exact outcome，即 `condition_id + asset`；`32°C Yes` 的卖压不影响 `33°C Yes` 的 BUY。
  - 冷却内同 outcome 的小额 BUY 视为试探/库存回补/碎片成交，跳过。
  - 冷却内明显大额 BUY 只发“可能重新建仓”提醒，初版不自动跟，等冷却结束后恢复正常评估。
- 新增参数：
  - `WEATHERHK_SOURCE_FLOW_WINDOW_SECONDS=120`：统计同 outcome 短窗口交易流。
  - `WEATHERHK_POST_SELL_BUY_GUARD_SECONDS=120`：单笔 SELL 后同 outcome BUY 保护期。
  - `WEATHERHK_SOURCE_PRESSURE_COOLDOWN_SECONDS=300`：短线卖压冷却时间。
  - `WEATHERHK_SOURCE_PRESSURE_MIN_SELL_COUNT=3`：短窗口内至少 3 笔 SELL 才考虑卖压。
  - `WEATHERHK_SOURCE_PRESSURE_MIN_SELL_NOTIONAL_USD=3`：短窗口 SELL 合计至少 3U。
  - `WEATHERHK_SOURCE_PRESSURE_MAX_AVG_SELL_GAP_SECONDS=30`：SELL 平均间隔不超过 30 秒，识别为高频/程序化特征。
  - `WEATHERHK_SOURCE_REENTRY_ALERT_BUY_USD=30`：冷却内 BUY 达到 30U 时按“可能重新建仓”提醒，但不自动跟。

### 10. 跟单金额分档提高到 2x

- 观察：跟随 WeatherHK 约一周后整体盈利，但收益捕获比例明显低于 WeatherHK 本人。
- 调整：
  - 原 `1-10U` 跟单档位整体提高为 `2-20U`。
  - 对 `<10U` 档保留“不超过 WeatherHK 本笔 BUY 金额”的保护：例如 WeatherHK 只买 `1U`，我方也只跟 `1U`，不强行放大到 `2U`。
  - `WEATHERHK_MAX_SINGLE_COPY_USD` 默认同步提高到 `20`，避免最高档被配置上限截回 `10U`。
- 风险控制不变：单市场敞口、每日投入、每日亏损、价格追价、高价薄利过滤仍继续生效；低价价格过滤默认关闭。

### 11. 低价 BUY 不再按价格直接跳过

- 观察：WeatherHK 的 `<1c` 或接近 0 的低价 BUY 可能是收益来源之一，直接按低价忽略会漏掉有效尾部机会。
- 调整：
  - `WEATHERHK_SKIP_BUY_PRICE_AT_OR_BELOW` 默认从 `0.005` 改为 `0`。
  - 本地实盘 `.env` 同步改为 `0`，即默认不启用低价价格过滤。
- 注意：CLOB 下单价格仍有最小价格约束，低于 `1c` 的源单可能只能以交易所允许的最低价/当前盘口尝试，因此仍依赖单市场敞口、每日额度和追价上限保护。

### 12. 页面合并成交识别

- 观察：Polymarket 页面个人记录会把短时间多笔小额成交显示成合并记录，常见于挂单被连续吃单。
- Data API 现状：
  - `/activity` 返回的是拆开的原始成交行，字段包括 `timestamp`、`side`、`price`、`size`、`conditionId`、`asset`、`transactionHash`。
  - 当前原始 `/activity` 没有直接的 UI 合并 id 或 `mergeable` 字段；`positions` 里的 `mergeable` 与 UI 交易记录合并不是同一件事。
- 可检测方案：
  - 按 `conditionId + asset + side` 绑定 exact outcome。
  - 在短窗口内按相同/接近 `timestamp`、接近价格、连续小额 `size/notional` 聚类。
  - 对 SELL 聚类用于识别挂单被吃/程序化出货。
  - 对 BUY 聚类应先合并金额再判断 `<1U` 阈值，避免多笔碎片 BUY 单独看都小于 `1U`，合并后其实是有效入场。

### 13. 改为 50% 比例跟单且不跳过小额 BUY

- 状态：已被第 14 条“严格 50% 且无本地最小跟单金额门槛”取代。
- 背景：WeatherHK 许多小额成交可能是低价挂单被吃，逐笔 `<1U` 跳过会错过尾部优势；同时用户不希望每日/单市场 cap 到顶后完全错过新机会。
- 调整：
  - `WEATHERHK_MIN_BUY_SOURCE_NOTIONAL_USD=0`，源 BUY `<1U` 不再默认跳过。
  - BUY 金额改为源金额 `50%`。
  - 如果 `50%` 小于 `0.01U`，视为不可拆小额，按源单全额跟。
  - 如果当前盘口没及时跟上而转为挂单，挂单金额仍按源总额 `50%`。
  - `WEATHERHK_MAX_SINGLE_COPY_USD`、`WEATHERHK_MAX_MARKET_EXPOSURE_USD`、`WEATHERHK_MAX_DAILY_SPEND_USD`、`WEATHERHK_MAX_DAILY_LOSS_USD` 在实盘配置中提高到近似不限制，让钱包余额成为自然上限。
- 保留：
  - `>98c` 高价薄利单不跟。
  - 低价价格过滤保持关闭。
  - 首笔 SELL 即时风控、短线卖压 exact outcome 冷却继续生效。

### 14. 取消本地最小跟单金额门槛

- 背景：继续观察后确认，WeatherHK 的极小额 BUY 也可能来自最低价挂单被吃；如果按 `0.01U` 做本地最小跟单门槛，仍会漏掉一部分尾部信号。
- 调整：
  - BUY 跟单金额严格按源成交金额 `50%`。
  - 不再因为计算后的跟单金额低于 `0.01U` 而跳过。
  - 不再对极小单做“全额跟”的特殊放大，避免偏离 50% 比例。
- 注意：如果 CLOB/执行器拒绝极小订单，结果应记录为执行失败，而不是 watcher 提前跳过；这有利于区分“策略不跟”和“交易所实际限制”。

### 15. 已结算市场的幽灵仓位重复清仓

- 现象：WeatherHK 和我方页面都已没有某个过期市场仓位，但本地状态仍保留历史份额；源仓位对账反复尝试 SELL，CLOB 返回 `invalid token id`，Telegram 周期性发送清仓失败。
- 原因：`invalid token id` 已经说明该 token 不可交易，但旧逻辑只把它当成不可重试失败并保留本地仓位；6 小时动作冷却结束后仍会再次尝试。
- 修复：
  - SELL 失败记录为 `invalid token id` 时，将对应本地仓位份额和敞口归零，并删除失败记录。
  - 源仓位对账现场遇到同类错误时立即静默清理，不再发送 failed Telegram。
  - 网络错误、无买盘和暂时无法成交仍保留仓位并按原逻辑重试，避免误删正常持仓。

### 16. 挂单部分成交消息与生命周期修复

- 现象：挂单只成交一部分时，Telegram 可能只显示笼统的 `filled`，看不到本次成交、累计成交和剩余挂单，也无法判断挂了多久。
- 修复：
  - 同步消息区分“挂单部分成交”和“挂单全部成交”。
  - 显示本次新增成交金额/份额、累计成交比例、剩余金额/份额。
  - 保存 WeatherHK 原始 BUY 时间，显示员工成交距今、我方挂单距今和挂单等待时长。
  - `filled` 但累计成交未达到原挂单总额时，内部状态改回 pending，保留剩余挂单。
  - 修复执行器把 `unmatched` 因包含 `matched` 而误判为 filled 的状态解析问题。

### 17. 同 outcome 小额 BUY 改为累计目标补单

- 现象：WeatherHK 的挂单可能在 1-3 分钟内拆成多笔小额成交；逐笔按 50% 下单会低于交易所最小 `5份`，但逐笔放大到 `1U` 又会造成三笔小单跟成 `3U`。
- 修复：
  - 按 exact outcome（`conditionId + asset`）累计 WeatherHK 本轮 BUY 金额。
  - 我方目标为累计金额的 `50%`；低价场景允许建立一次性 `1U` 启动目标，但只有最坏追价下 `1U` 仍至少能买到 `5份`时才启用。
  - 我方已成交仓位成本与未成交挂单余额共同计入“已承诺金额”，每次只补目标缺口。
  - 缺口不足最坏追价下的交易所最低 `5份`时先累计，不提交必然失败的订单。
  - WeatherHK 对同 outcome 出现 SELL 时重置这一轮累计 BUY 目标，并继续执行撤单/退出逻辑。

### 18. 退出重试先按真实余额对账

- 现象：网络异常后本地仍记录旧仓位，实际 CLOB 余额已经接近零；退出重试会显示“卖出旧份额”，随后因真实余额低于 `5份`而 skipped，形成误导消息。
- 修复：
  - 执行器每次 SELL 仍先读取真实 CLOB token 余额。
  - 真实余额为零或低于最低 `5份`且执行器返回 dust/zero skipped 时，本地仓位归零并清除退出失败任务。
  - 该对账结果静默处理，不发送“退出重试卖出”Telegram，因为没有发生新的卖出。
- 真实余额仍可交易时，继续按真实余额执行退出；网络异常和暂无买盘仍保留重试。

### 19. SELL 改为按双方实际仓位同步减仓

- 检测到 WeatherHK 第一笔 SELL 后立即取消同 outcome 未成交 BUY，不等待频率统计。
- 立即刷新 WeatherHK `/positions`，以“上次实际份额 - 当前实际份额”计算净减仓比例；同批多笔 SELL 已反映在一次仓位变化中时不重复卖。
- 我方卖出数量由执行器读取真实 CLOB token 余额后乘以该比例，不再以本地持仓账本作为下单依据。
- WeatherHK 实际仓位已归零，或净减仓达到 `40%`，我方立即清仓；小比例减仓用 FOK 按保护价同步执行。
- WeatherHK 成交价达到 `99c` 以上时启用锁盈保护，目标最低卖价 `99.8c`；执行器按该市场 tick 向下修正，例如 `1c` tick 时使用合法的 `99c`。
- 比例卖出低于交易所最低 `5份`、但我方余额仍可交易时，向上取到 `5份`立即执行，并在执行器消息中明确说明。
- 网络失败重试保留原减仓比例和最低卖价，不会把小比例减仓错误升级为全清。

### 20. 高频扫描延迟修复

- 原因：`1秒`只是配置轮询间隔；旧循环会在 `/activity` 前串行执行挂单同步、退出重试和源仓位对账，单请求最长 `20秒`，多个任务叠加后可延迟数分钟。
- 调整：每轮先请求 `/activity` 并处理新交易，维护任务移动到交易扫描之后；`handle_trade` 内不再隐式执行维护任务。
- 每轮最多同步 2 个 pending 订单，避免大量挂单同步长期占住主循环。
- 高频 `/activity` 和 `/positions` 请求改为 `4秒`连接超时、`8秒`总超时；Telegram 同步发送也从 `20秒`降为 `8秒`总超时。
- 轮询改为目标周期计时：API 请求已经消耗的时间不再额外固定睡 `1秒`。当前代理实测单次 Data API 请求约需数秒，因此无法保证每秒得到新响应，但不会再人为多等一轮。
- 自动跟随消息增加“检测延迟”字段，便于区分 Data API 暴露延迟、网络耗时和执行器耗时。

### 21. 延迟 SELL、FOK 失败和后续 SELL 被冷却拦截

- 现场：WeatherHK 先卖出 `10.3291份`，但 `/activity` 晚到时定期 `/positions` 已经是卖出后的 `7.8863份`；旧逻辑看到前后快照相同，误判为无需减仓。随后 `1.0870份`的小比例 SELL 使用 FOK，盘口无法一次全部成交而整单失败；第三笔接近清仓的 SELL 又被前一笔失败产生的 `900秒`冷却挡住，只剩普通员工提醒。
- 修复：
  - 当 SELL 事件到达而实际仓位快照已经相同或更靠后时，用 `当前实际剩余 + 本笔 SELL 份额`重建卖出前仓位，不再把延迟事件判为零减仓。
  - 同一轮只刷新一次 WeatherHK `/positions`，避免每笔碎片 SELL 反复请求并得到不同时间点快照。
  - 对一次实际仓位净下降超过当前碎片 SELL 的部分记录短期“批次覆盖量”；后续同批碎片只消耗覆盖量，不重复卖出。
  - 新的 WeatherHK SELL 永远不受旧 SELL 执行失败冷却阻挡，每笔新风险信息都立即尝试执行。
  - 比例减仓由 FOK 改为带最低卖价保护的 FAK，允许先部分成交；剩余不少于交易所最低 `5份`时记录精确剩余份额并重试。
  - 若前一笔减仓失败后又出现新的 SELL，将未完成份额合并到新动作，不丢失此前应卖数量。
  - 兼容旧版本留下的 FOK 失败任务；升级后会重新读取我方真实余额。若用户已手动清仓，则静默把本地仓位归零。

### 22. `>90c` 高价 BUY 仓位封顶 50U

- 背景：90c 以上赔率单利润空间有限，继续按 50% 跟随大额 BUY 容易占用大量本金，且风险收益比下降。
- 规则：
  - 源成交价 `>90c` 且 `<=98c` 时，先计算 WeatherHK 累计 BUY 的 50% 跟单目标，再把我方该 exact outcome 目标封顶为 `50U`。
  - WeatherHK 累计买入 `70U / 100U / 200U` 时，我方目标分别为 `35U / 50U / 50U`。
  - 已成交成本与未成交 BUY 挂单余额共同计入 50U，拆成多笔成交或挂单也不能突破上限。
  - `90c` 及以下保持原 50% 逻辑；`>98c` 仍直接跳过。
  - 如果此前在低价阶段建立的仓位已经超过 50U，后来出现 90c 以上 BUY 时只停止继续加仓，不主动减仓到 50U。
- 配置：`WEATHERHK_HIGH_PRICE_EXPOSURE_THRESHOLD=0.90`，`WEATHERHK_HIGH_PRICE_EXPOSURE_CAP_USD=50`。

### 23. `>90c` 高价 BUY 禁止向上追价

- 背景：98c 的源成交如果继续使用通用 `+15%` 追价，会被抬到 99c；这会吃掉几乎全部收益，还可能在临近结算时承担不对称风险。
- 规则：
  - 源成交价 `>90c` 且 `<=98c` 时，最高买价默认等于 WeatherHK 的源成交价，不再使用通用 `+15%` 追价。
  - 当前卖一不高于源成交价时可以立即 FOK；卖一更贵时只按源成交价 post-only 挂单等待。
  - 例如 WeatherHK 成交在 `95c / 98c`，我方最高只买到 `95c / 98c`，不会追到 `99c`。
  - `90c` 及以下仍使用通用追价参数；`>98c` 仍直接跳过；高价 exact outcome 的 `50U` 仓位上限继续生效。
- 配置：`WEATHERHK_HIGH_PRICE_MAX_CHASE_PCT=0`。若以后经过观察确需允许极小幅追价，可单独调整，不影响低价单的通用追价比例。

### 24. `/activity` 热扫描与维护任务隔离

- 现场：虽然配置为 `1s` 扫描，但旧 watcher 在每轮 `/activity` 后串行执行 Telegram、挂单同步、SELL 退出重试和 `/positions` 对账。运行日志显示每小时实际只完成约 `115-180` 次轮询，平均 `20-31s` 才开始下一轮；常见跟单总延迟为 `30-70s`，网络异常时达到数分钟。
- 改造：
  - 独立 activity poller 持续请求 `/activity`，只负责去重并把新交易送入执行队列，不再等待其他维护工作。
  - WeatherHK `/positions` 改由独立只读线程缓存；SELL 只使用“采集时间不早于该笔交易且不超过 30 秒”的缓存，旧缓存不会参与减仓比例判断。
  - Telegram 改为容量 1024 的异步队列；发送超时不再阻塞扫描或下单。队列极端满载时丢弃通知并写 stderr，交易执行优先。
  - 自动跟单状态和 CLOB 下单仍由单一线程串行持有，避免同 outcome 重复下单、BUY/SELL 乱序和状态文件并发覆盖。
  - 挂单同步、过期撤单和历史 SELL 重试改为低优先级单步维护：交易队列安静至少 2 秒后才运行，每 5 秒最多启动一个外部动作，SELL 重试优先。
  - 源仓位对账每 30 秒最多处理一个撤单或清仓动作，不再一次遍历并执行全部历史任务。
  - 跟单消息新增延迟拆分：`API/轮询发现`、`本地排队`、`执行耗时`和本次 `/activity` 请求耗时，便于继续定位 Data API 与本地执行各自的影响。
- 预期：Data API 正常返回时，本地发现轮询恢复到接近配置频率；维护任务不再造成稳定的几十秒扫描空窗。实际总延迟仍受 Polymarket Data API 发布时间、代理质量和 CLOB 执行耗时影响。
- 2026-06-14 部署后首分钟实测：代理持续出现 `SSL connection timeout` 并触发 3/5 秒退避时，仍完成 `22` 个调度周期、`15` 次实际 employee `/activity` 请求；相比旧版约 `2-3次/分钟` 提高约 `5-7倍`。当前剩余主要瓶颈是代理/Data API 网络，而非维护任务阻塞。

### 25. 新增 `OlympusHive` 独立 50% 跟单实例

- 新 source wallet：`0xf421705cbe3dd07db21ddd4a61eb8cce9386efce`。
- 运行方式：单独 LaunchAgent `com.smartwallet.weatherf421.autocopy`，不与 WeatherHK 共用状态文件。
- 员工昵称：`OlympusHive`；脚本和 Telegram 展示使用昵称，LaunchAgent/state 文件名暂保留 `weatherf421` 技术名，避免丢失已成交/挂单状态。
- 策略：复用 WeatherHK 自动跟单规则，但显式关闭小额 BUY 的 `100%` 放大，BUY 严格按源成交累计金额的 `50%` 跟随；例如源在 5c 买入 `10U`，我方目标本金是 `5U`，即使追到 6-8c 也只围绕 `5U` 下单，买到的份额相应减少。`>98c` 跳过，`>90c` outcome 目标封顶 `50U` 且不向上追价。
- 状态文件：`logs/weatherf421_autocopy_state.json`。
- 启动脚本：`/Users/will/Library/Application Support/smart-wallet-discovery/start_weatherf421_autocopy.sh`。
- 日志：
  - stdout：`~/Library/Logs/smart-wallet-discovery/weatherf421-autocopy-launchd.log`
  - stderr：`~/Library/Logs/smart-wallet-discovery/weatherf421-autocopy-launchd.err`
- 注意：两个跟单实例共用同一个实盘钱包资金，但单市场/每日额度状态彼此独立；如需跨员工共享总资金上限，需要后续新增全局风控账本。

### 26. 高价仓位对账补挂也必须套 50U cap

- 现场：OlympusHive 在高价区间（例如 `94.5c`）的大额持仓，被源仓位对账补挂路径按“源持仓成本金额 * 50%”追补，绕过了实时 BUY 路径已有的 `>90c` exact outcome `50U` 上限，导致高价低收益单占用过多本金。
- 修复：
  - 源仓位对账补挂先计算金额比例目标，再使用最近补仓/买入价与持仓均价中的较高值作为高价参考价。
  - 参考价 `>90c` 时，该 exact outcome 的风控后目标金额封顶 `50U`。
  - 提示文案同时展示“金额原始目标”和“风控后目标”，避免误解为单纯份额 50%。
- 注意：本次只修复后续自动动作，不主动处理已经发生的那笔高价仓位/挂单；该单由用户手动观察。

### 27. 高价风控必须按我方可能入场价触发

- 现场：WeatherHK 源成交价 `88.65c`、买入约 `408.78U`，普通追价允许我方在 `93c` FOK 成交，最终我方买入约 `209U`。虽然源价未超过 `90c`，但我方实际入场价已经超过 `90c`，风险收益已经进入高价低收益区间。
- 修复：
  - 实时 BUY 不再只看源成交价是否 `>90c`。
  - 如果普通直接追价/被动挂价会让我方入场价越过 `90c`，则触发高价风控。
  - 触发后 exact outcome 目标金额封顶 `50U`，且源价未超过 `90c` 时最高买价压到 `90c`，不能再 FOK 吃到 `93c/94.5c`。
  - 对源价本身已经 `>90c` 的单，仍按原高价逻辑：最多 `50U`，不向 `99c/100c` 追价。

### 28. 全员 BUY 目标统一封顶 50U 与余额不足降额挂单

- 新规则：
  - 不论 WeatherHK、OlympusHive 或后续新增员工，BUY 目标先按员工源金额策略计算，再把同 exact outcome 的最终目标封顶为 `50U`。
  - 已成交仓位和未成交 BUY 挂单共同计入目标；员工拆单连续买入时只补目标缺口，不能每笔都重新买到 50U。
  - 当前已有仓位不主动卖出、不主动调仓；如果某个 outcome 已经超过 50U，后续 BUY 只会停止继续加仓。
- 执行器修复：
  - BUY 前读取 CLOB collateral balance/allowance。
  - 如果本次目标金额超过可用资金，自动降到可用资金减小缓冲后可下单的金额，并强制走 post-only 挂单，不再直接 FOK 失败。
  - 如果剩余可用资金低于交易所最低约 `1U`，才跳过并说明余额不足。
  - post-only 挂单提示固定包含我方挂单价和当前卖一，方便观察员工买入后盘口变化。
- 配置：
  - `WEATHERHK_COPY_TARGET_CAP_USD=50`
  - `POLYMARKET_BUY_BALANCE_BUFFER_USD=0.05`

## 运行与部署

- LaunchAgent：`com.smartwallet.weatherhk.autocopy`
- 本地启动脚本模板：`scripts/start_weatherhk_autocopy_appsupport.sh`
- App Support 运行脚本：`/Users/will/Library/Application Support/smart-wallet-discovery/start_weatherhk_autocopy.sh`
- 状态文件：`logs/weatherhk_autocopy_state.json`
- stdout 日志：`~/Library/Logs/smart-wallet-discovery/weatherhk-autocopy-launchd.log`
- stderr 日志：`~/Library/Logs/smart-wallet-discovery/weatherhk-autocopy-launchd.err`

## 待观察

- Telegram 429 后需要等待冷却，避免重复 pending 通知。
- SELL 失败冷却只是止血；如果继续出现 `not enough balance / allowance`，需要核对实际 CLOB 持仓/allowance 与本地状态文件是否一致。
- `/activity` 比 `/trades` 更实时，但仍需继续观察是否存在漏单。
- 若仍出现 `Request exception`，需要检查代理/CLOB 网络质量；executor 已会对短暂抖动做有限重试。
- 对天气阶梯市场，当前短线卖压保护严格限定 exact outcome；后续如要做同城市/同日期/相邻温度档联动，需要单独观察和设计，避免误伤机会。
