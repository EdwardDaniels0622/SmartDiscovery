# OlympusHive 实际 API 复盘

更新时间：2026-07-15

## 数据来源

- OlympusHive wallet：`0xf421705cbe3dd07db21ddd4a61eb8cce9386efce`
- Activity：`data-api.polymarket.com/activity`
- Closed positions：`data-api.polymarket.com/closed-positions`
- 市场最终结果：`gamma-api.polymarket.com/events?slug=...`
- 拉取方式：通过本机代理 `http://127.0.0.1:7890`，失败时脚本尝试直连 fallback。

## 口径说明

本报告有两个口径：

1. **Activity 现金流估算**
   - 用最近 5 天实际 BUY/SELL activity 计算：`卖出收入 + 剩余净份额 * 最终价格 - 买入成本`。
   - 优点：能看到同 event 内多个 outcome 的买卖轮动，尤其是买了又卖掉的低价档位。
   - 限制：如果某个 event 的仓位在 2026-07-10 00:00 之前已经建立，则 5 天窗口会漏掉早期成本，估算会偏差。

2. **Closed positions realizedPnl**
   - Data API 直接返回 `realizedPnl`，更接近 Polymarket 的官方持仓结算口径。
   - 优点：能验证哪些城市/最终档位实际赚亏。
   - 限制：对于同 event 内已经买卖清空的非最终持仓，可能不会完整反映 event 内所有交易现金流。

因此，讨论策略形态时优先看 activity；讨论已结算胜负时参考 closed positions 和 Gamma 结果。

## 实际 Activity 拉取结果

时间范围：2026-07-10 00:00:23 到 2026-07-15 10:21:31（上海时间）

- 实际 TRADE：697 笔
- Event：51 个
- Gamma 查询成功：51 个 event，0 个 event fetch error
- 已 closed：41 个
- 仍 open：8 个

## 近期主要 Activity Event

| Event | 买入 | 卖出 | 买/卖笔数 | Outcome 数 |
|---|---:|---:|---:|---:|
| Seoul July 10 | 546.11U | 507.63U | 13/4 | 4 |
| Seoul July 14 | 474.55U | 834.65U | 37/40 | 4 |
| Shenzhen July 13 | 461.54U | 363.62U | 25/10 | 3 |
| NYC July 14 | 331.01U | 9.24U | 33/2 | 6 |
| NYC July 9 | 320.10U | 719.24U | 8/9 | 4 |
| Houston July 9 | 266.55U | 153.47U | 4/8 | 3 |
| Hong Kong July 14 | 228.34U | 592.83U | 15/11 | 2 |
| Austin July 9 | 175.93U | 381.24U | 96/7 | 3 |
| Shenzhen July 14 | 160.04U | 0.00U | 11/0 | 1 |
| Miami July 9 | 142.71U | 161.49U | 12/8 | 3 |
| Beijing July 10 | 103.22U | 0.00U | 11/0 | 5 |
| NYC July 10 | 101.86U | 0.00U | 66/0 | 4 |
| NYC July 13 | 87.85U | 82.65U | 8/7 | 3 |
| Chicago July 9 | 86.16U | 172.64U | 24/3 | 3 |
| NYC July 15 | 78.65U | 0.00U | 2/0 | 2 |
| Warsaw July 14 | 68.49U | 33.81U | 12/8 | 4 |
| Shenzhen July 10 | 67.77U | 5.10U | 25/4 | 3 |
| Seoul July 13 | 54.08U | 0.65U | 5/1 | 1 |
| Amsterdam July 13 | 52.34U | 39.58U | 5/1 | 2 |
| Hong Kong July 15 | 48.02U | 0.00U | 1/0 | 1 |
| Chicago July 15 | 47.76U | 0.00U | 7/0 | 3 |
| Shenzhen July 15 | 47.36U | 0.00U | 16/0 | 2 |

这直接证明：OlympusHive 经常不是只买一个温度档位，而是同一城市同一天多个 outcome 同时布局。

## 最终结果与员工盈亏

### Hong Kong July 14

- 最终结果：28C
- Activity 估算：+364.51U
- Closed positions：+384.90U
- 核心仓位：28C Yes，avg 19.36c，官方 realizedPnl +384.90U。
- 低价尾仓：29C Yes，约 13.97U，最终归零。

结论：这是非常成功的一组主仓胜利。即使低价尾仓亏掉，主仓收益完全覆盖。

### Seoul July 14

- 最终结果：32C
- Activity 估算：+360.11U
- Closed positions：+224.83U
- 档位：
  - 32C Yes：买 231.93U，卖 446.82U，最终 Yes=1，activity 估算 +214.89U。
  - 29C Yes：买 216.12U，卖 331.82U，最终 Yes=0，但提前卖出，activity 估算 +115.70U。
  - 33C Yes：买 24.44U，卖 56.01U，最终 Yes=0，但提前卖出，activity 估算 +31.57U。
  - 34C Yes：买 2.06U，未卖，最终归零。

结论：这个 event 最能说明 OlympusHive 的强项。他不只是买中 32C，还在错误档位 29C/33C 上通过价格波动提前卖出赚钱。

### Chicago July 9

- 最终结果：86-87F
- Activity 估算：+86.47U
- Closed positions：+110.03U
- 结构：
  - 86-87F Yes：核心盈利，activity 估算 +109.44U。
  - 88-89F / 82-83F：辅助档位亏损约 22.96U。

结论：多档位中一个主档位命中，辅助档位亏损可控。

### NYC July 9

- 最终结果：86-87F
- Activity 估算：+61.05U
- Closed positions：+60.14U
- 结构：
  - 84-85F No：盈利约 +48.60U。
  - 86-87F Yes：接近打平。
  - 88-89F Yes：提前卖出盈利。

结论：不是只买 Yes，也会通过 No 仓做范围排除或保护。

### Miami July 9

- 最终结果：90-91F
- Activity 估算：+18.78U
- Closed positions：+29.01U
- 结构：
  - 90-91F Yes：盈利。
  - 92-93F Yes：亏损。
  - 94-95F Yes：小额彩票仓归零。

结论：组合仓，命中主档位，其他档位作为成本。

### Shenzhen July 13

- 最终结果：34C
- Activity 估算：-97.92U
- Closed positions：-90.78U
- 结构：
  - 33C No：盈利约 +19.96U。
  - 34C Yes：盈利约 +5.88U。
  - 32C Yes：亏损约 -123.76U。

结论：这是失败案例。说明他不是无敌，错误主仓会吞掉其他对冲/命中仓收益。

### Shenzhen July 14

- 最终结果：29C
- Activity 估算：-160.04U
- Closed positions：暂未在最近 closed positions 中看到对应盈利项。
- 结构：
  - 28C Yes：买入约 160U，最终归零。

结论：单档位重仓错误，亏损很大。对我们来说，这种单档位追随风险高。

### Beijing July 10

- 最终结果：27C or below
- Activity 估算：-103.22U
- 多档位买入 29C/30C/31C/32C/33C，全部归零。

结论：多档位也可能整体方向错，不能把“多档位”简单等同于安全。

## 城市表现：Closed Positions 口径

| 城市 | Event 数 | realizedPnl |
|---|---:|---:|
| Hong Kong | 3 | +422.44U |
| Seoul | 3 | +186.92U |
| Chicago | 1 | +110.03U |
| New York City | 2 | +56.02U |
| Miami | 1 | +29.01U |
| Amsterdam | 1 | +27.60U |
| Munich | 1 | +24.51U |
| Shenzhen | 2 | -113.38U |
| Houston | 1 | -104.00U |
| Warsaw | 2 | -40.96U |
| Beijing | 1 | -103.22U |

初步看，OlympusHive 最近实际盈利主要来自 Hong Kong、Seoul、Chicago；深圳最近反而表现差，尤其 Shenzhen July 13/14。

## 对我们跟单策略的启发

### 1. 不能只跟主仓

Seoul July 14 说明：他在最终错误的 29C/33C 上也能通过提前卖出赚钱。如果我们只跟到 32C 或只漏掉低价/中间档位，就不是同一个策略。

### 2. 低价仓不是装饰

NYC、Chicago、Seoul 都出现多个低价 outcome。低价仓有两个作用：

- 提供赔率弹性；
- 在盘中波动时可以卖出获利，即使最终不是 winner。

我们漏低价仓，会直接失去这部分收益来源。

### 3. Event-level 比 trade-level 更合理

实际数据支持按 event 建篮子：

- Seoul July 14：4 个 outcome。
- NYC July 14：6 个 outcome。
- Chicago July 15：3 个 outcome。
- Shenzhen July 15：2 个 outcome。

下一版应先构造员工 event basket，再决定我方 basket。

### 4. 城市和日期也要进入风控

最近结果显示：

- Hong Kong / Seoul 更值得优先保留篮子形状。
- Shenzhen 最近亏损明显，不能因为它交易多就盲目加权。
- 单档位大额错方向风险很高，如 Shenzhen July 14。

### 5. 挂单必须随 basket 变化

如果员工从一个档位转向另一个档位，我们旧挂单继续存在就会变成接过时仓。正确逻辑应是：

- 同 event 最新 basket 发生明显重心迁移；
- 旧 outcome 不再被源 basket 支持；
- 我们的旧补买挂单应取消或降级。

## 暂定改造判断

实际 API 数据支持用户提出的问题：

- 当前本地账本确实只能说明我们处理了什么；
- 真实 Polymarket 数据显示 OlympusHive 最近多次通过 event-level 多档位组合赚钱；
- 漏掉低价/次级档位会让我们的收益和员工收益严重分叉；
- 单纯 50% trade-level 跟单不是合适模型。

下一步应讨论 event-level 跟单规则，而不是继续补单笔逻辑。
