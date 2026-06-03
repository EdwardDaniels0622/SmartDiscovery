# WeatherHK 自动跟随记录

本文档记录 WeatherHK 小额自动跟随的需求、实现取舍和已修复问题，方便后续回滚、复盘和版本管理。

## 当前目标

- 只对 `WeatherHK` 钱包启用自动跟随。
- 用小额资金复制其 WEATHER 市场买卖节奏，验证能否跟到短线优势。
- 保持单笔、单市场、每日投入和每日亏损上限。
- 私钥和 funder 地址只放本地 `.env`，仓库只提交 `.env.example`。

## 当前核心规则

- 实时数据源使用 Polymarket Data API `/activity`，不再用 `/trades` 做实时 watcher。
- WeatherHK 常驻 watcher 使用 `--trade-limit 100`。
- 源 BUY 金额 `< 1U` 默认跳过。
- 高价低收益 BUY 默认跳过：`>= 98c`。
- 近零尾部 BUY 默认跳过：`<= 0.5c`。
- 跟单金额按 WeatherHK 源成交分档：
  - `<10U => 1U`
  - `10-30U => 2U`
  - `30-60U => 3U`
  - `60-100U => 5U`
  - `100-200U => 8U`
  - `>=200U => 10U`
- BUY 执行：
  - 如果当前卖一在直接追价上限内，允许小额 FOK 买入。
  - 如果卖一超出直接追价上限或不可用，则挂 post-only 限价单。
- SELL 执行：
  - WeatherHK SELL 先取消同 asset 的 pending BUY。
  - 小额 SELL 仍取消 pending BUY，但低于阈值时不自动同步卖出我方已成交仓位。
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
- 修复：默认跳过 `>=98c` 的 BUY。

### 4. 0c/极低价格尾部单被跟随

- 现象：接近 0 的尾部成交也可能触发跟单。
- 风险：极小概率尾部或 stale maker 噪音。
- 修复：默认跳过 `<=0.5c` 的 BUY。

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
- 对天气阶梯市场，可以继续评估“同城市/同日期/同温度系列”联动撤单规则，但第一版先以同 asset 和源持仓对账为主。
