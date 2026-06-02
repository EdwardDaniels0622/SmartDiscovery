# Smart Wallet Discovery

Polymarket smart-wallet discovery system scaffold.

This project is intentionally read-only. It is designed to find and score wallets first, then emit structured discovery signals that a future Rust copy-trading executor can consume. No private keys, no signing, no order placement.

See [docs/requirements.md](docs/requirements.md) for the product requirements document.

## Goal

Build a candidate pipeline for wallets that appear to have repeatable information edge, while filtering out wallets whose historical PnL is hard to copy:

- one-off lucky winners
- late-entry certainty buyers
- market makers and spread-capture flows
- high-hedge wallets
- wallets trading markets too thin for followers

## Data Sources

Initial Polymarket adapters should map to public, read-only endpoints:

- Leaderboard and user activity: <https://docs.polymarket.com/api-reference/core/get-trader-leaderboard-rankings>
- User or market trades: <https://docs.polymarket.com/api-reference/core/get-trades-for-a-user-or-markets>
- Market data and order books: <https://docs.polymarket.com/api-reference/market-data/get-order-book>
- Gamma market metadata: <https://docs.polymarket.com/api-reference/gamma/markets>

The interfaces live in `src/ports.rs`, so the scoring core does not care whether data comes from REST, cached files, SQLite, or a streaming indexer.

## Scoring Model

The first scoring pass combines:

- realized profitability: ROI adjusted by drawdown
- consistency: positive-month ratio and resolved-market sample size
- entry edge: closing-line value, median entry price, and late-entry ratio
- low hedge behavior: filters frequent YES/NO same-market exposure
- liquidity replicability: estimates whether a follower can actually enter near the leader's price
- recency: avoids dead wallets
- category focus: rewards specialized domain edge

The model is deliberately simple and inspectable. We can make it more statistical later once we have cached real wallet data.

## Future Copy-Trading Interface

Future execution should consume `DiscoverySignal` from `src/model.rs`:

- `source_wallet`
- `market_id`
- `asset_id`
- `side`
- `observed_price`
- `max_follow_price`
- `confidence`
- `suggested_budget_usd`
- `expires_at_ms`
- `reasons`

The executor should still apply its own risk checks:

- jurisdiction and account eligibility
- max exposure per wallet
- max exposure per market
- max price drift from observed trade
- order-book depth and slippage simulation
- kill switch and dry-run mode

## Run

```bash
cargo test
cargo run
cargo run -- discover
cargo run -- scan-employees --top 10
```

Useful discovery flags:

```bash
cargo run -- discover --categories SPORTS,CRYPTO,POLITICS --candidate-limit 8 --closed-pages 2
cargo run -- discover --json
cargo run -- discover --proxy socks5h://127.0.0.1:7891
cargo run -- discover --proxy none
cargo run -- discover --recent-window-days 30 --min-recent-pnl 0 --min-recent-roi 0
cargo run -- discover --time-period ALL --max-current-loss 100000
```

The simplified discovery command pulls public Polymarket leaderboard entries, samples closed positions and current positions, and picks one wallet per category by default. The default leaderboard period is `MONTH`, because all-time leaders can be washed after a bad recent run. It marks candidates as `HIRE` only when they pass the rough employee policy: enough closed markets, positive sampled PnL and ROI, controlled drawdown, recent activity, no recent cold streak, positive recent-window performance, and no large current/open loss. By default the recent window is 30 days, with at least 3 recent closed positions, non-negative recent PnL, non-negative recent ROI, at least 45% recent win rate, current/open loss below $50,000, and current/open loss ratio below 20%. API requests use `http://127.0.0.1:7890` as the default proxy.

If the default Data API endpoint or proxy should be changed, override them with:

```bash
POLYMARKET_DATA_API_BASE=https://data-api.polymarket.com POLYMARKET_PROXY_URL=http://127.0.0.1:7890 cargo run -- discover
```

## Semi-Auto Watcher

The semi-auto mode does not trade. It scans employees, watches their new BUY/SELL trades, and sends a Telegram alert when a trade looks manually followable or when an employee appears to be exiting a position.

Scan up to 10 employees:

```bash
cargo run -- scan-employees --top 10 --candidate-limit 5 --closed-pages 2
```

Watch the scanned employees for new trades:

```bash
TELEGRAM_BOT_TOKEN=... TELEGRAM_CHAT_ID=... cargo run -- watch --scan-top 10
```

Watch one hand-picked TECH/AI employee:

```bash
cargo run -- watch \
  --employee '0xd029b13f2e562c77e2db3e5eda140cee79a44fc1:bignewsagencypro:TECH:gemini|ai|llm|model|arena|openai|anthropic|xai|grok:30' \
  --poll-seconds 10 \
  --heartbeat-seconds 3600 \
  --min-notional 100 \
  --max-entry-price 0.75
```

The `--employee` format is `wallet:name:domain:keyword1|keyword2:poll_seconds:min_notional_usd`; the last two fields are optional. Use a short poll interval for high-frequency employees and a longer interval for medium/low-frequency employees. The notional threshold is now a reference level for alert explanation and priority, not a hard BUY filter after the trade matches an employee's specialist keywords. Very tiny dust trades are still ignored. The global `--poll-seconds` is the scheduler tick, not a forced API call for every employee.

By default, the watcher seeds its first poll as a watermark and only alerts on newly observed trades. BUY alerts explain whether the entry matches the employee profile. SELL alerts estimate the employee's known cost basis from recent same-outcome trades and classify the exit as `止盈减仓`, `止损撤退`, `调仓减仓`, or `未知卖出`. Heartbeats are aligned to wall-clock intervals, so the default hourly heartbeat fires around the top of the hour instead of one hour after startup. If Telegram credentials are not set, alerts and heartbeats are printed to stdout for dry-run.

Analyze employee activity frequency:

```bash
cargo run -- activity --trade-limit 100 --employee 'wallet:name:TECH:gemini|ai|llm'
```

Build employee trading profiles:

```bash
cargo run -- profiles \
  --employee 'wallet:name:SPORTS:nba|nfl|mlb|ufc|soccer|tennis' \
  --profile-trade-limit 100 \
  --profile-closed-pages 2
```

The `profiles` command summarizes how an employee tends to make money and manage exits: realized PnL/ROI, profit concentration, median and P80 trade size, SELL notional ratio, quick-flip ratio, best subcategories, best price bands, suspected market-making behavior, and strategy labels such as `重仓信号型`, `稳定小额型`, `冷门赔率型`, `提前卖出型`, `短线操作型`, and `高频做市/套利型`. Use `--json` when another process needs the full structured profile.

`watch` now loads the same profiles at startup by default and adds a copy-trade score, alert level, strategy summary, reasons, and cautions to each Telegram/stdout alert. Disable this with `--no-profiles` if you need the old lightweight watcher behavior.

## Next Build Step

The next useful module is a `PolymarketDataClient` adapter:

1. Pull candidates from leaderboard by category.
2. Pull wallet trades and activity.
3. Pull market metadata and order books.
4. Derive `WalletMetrics`.
5. Rank wallets with `score_wallet`.
6. Emit discovery signals only when a high-scoring wallet enters at a copyable price.
