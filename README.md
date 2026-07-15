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
cargo run -- recruit-employees --top 10 --trade-pages 5
cargo run -- scan-domain-employees --domain WEATHER --leaderboard-depth 500 --wallet-limit 500
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

## Tape-Based Employee Recruitment

`scan-employees` is the old leaderboard-first recruiter. The newer `recruit-employees` command starts from global `/trades` tape instead: it looks for BUYs that were copyable at entry, then checks whether later same-outcome tape moved in the buyer's favor. Repeated good trades are aggregated back into wallet + domain leads, then wallet-health vetting checks closed positions, recent PnL, current/open loss, two-sided footprints, and the recruitment reject list before anything is printed as a ready-to-watch employee spec.

```bash
cargo run -- recruit-employees --top 10 --trade-pages 5
cargo run -- recruit-employees --domains WEATHER,TECH --trade-pages 10
cargo run -- recruit-employees --min-tape-move 0.02 --min-candidate-trades 3
cargo run -- recruit-employees --max-current-loss 2500 --reject-wallets 0xabc,0xdef
scripts/recruit_employees_v2.sh --domains WEATHER
```

For scheduled recruitment, run the hourly scanner and daily summary:

```bash
scripts/recruit_employees_hourly.sh
scripts/recruitment_daily_summary.sh
```

The hourly scanner saves JSON reports under `logs/recruitment/` and writes a combined `logs/recruitment/recruitment-summary.md`. Rejected leads are kept in JSON under `rejected_candidates`, and the summary file shows recent wallet-health rejections so old false positives do not look like hires. By default the summary only selects candidates that include `wallet_health`, meaning old tape-only reports are treated as diagnostics unless `--allow-unvetted` is passed. For local background collection, install `launchd/com.smartwallet.recruitment.hourly.plist` so launchd runs the hourly scanner silently. Codex should only keep a daily reminder that reads the combined summary file, instead of posting every hourly scan.

See `docs/employee_recruitment_v2.md` for the requirements and scoring rationale.

## Rotating Leaderboard Recruitment

The current employee search returns to a leaderboard-first approach, but it does not assume that the first few ranks are copyable. `scan-domain-employees` merges `DAY`, `WEEK`, `MONTH`, and `ALL` leaderboards, paginates as far as rank 1000, and then deeply evaluates a configurable number of unique wallets.

The first rotation contains seven short-cycle domains:

```text
WEATHER, SPORTS, CRYPTO, FINANCE, ECONOMICS, TECH, MENTIONS
```

`POLITICS` is kept outside the first rotation because many political markets settle over weeks or months. `MENTIONS` is used instead of broad `CULTURE`, because recurring word/phrase markets more often settle daily or weekly.

The deep filter records total-wallet and domain-only PnL for 1/7/14 days. One-day PnL may be negative, while both 7-day and 14-day total and domain PnL must be positive. It also checks domain profit share, 14-day domain ROI and sample size, top-five profit concentration, profit earned from entries at 80c or above, and unresolved active-position loss. Redeemable positions are excluded from active risk.

For `CRYPTO`, ordinary employee metrics exclude 5-, 10-, and 15-minute markets. Detection uses explicit duration labels and actual title time ranges such as `3:30AM-3:45AM`. The report records the excluded position, profit, and invested-capital shares separately. Profitable ultra-fast wallets appear only under `Ultra-Fast Observation Only`; they are not employees until entry timing and 30/60-second delayed-copy performance are validated.

Run one domain manually:

```bash
cargo run -- scan-domain-employees --domain WEATHER --leaderboard-depth 500 --wallet-limit 500
```

Run the daily rotation. The first successful run starts with `WEATHER`, then advances through the seven-domain list. JSON snapshots and one append-only Markdown report are written under `logs/employee-discovery/`.

```bash
scripts/scan_employee_domains_daily.sh
```

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

## Cached Employee Deep Inspection

Use `employee-stats` when a candidate wallet needs a full 14-day review. The first refresh paginates Polymarket activity, trades, closed positions, and current positions, then stores the raw dataset and report in local SQLite. Later `show` calls are cache-only and do not call the API.

Deep-inspect a new address. `--domain` and `--keywords` are optional; when omitted, the primary domain is inferred and marked as inferred in the report.

```bash
scripts/employee_deep_check.sh refresh 0xabc... --format compact-json

cargo run -- employee-stats refresh \
  --wallet 0xabc... \
  --domain WEATHER \
  --keywords 'weather|temperature|hurricane|storm' \
  --format compact-json
```

Read the cached conclusion without network access:

```bash
scripts/employee_deep_check.sh show 0xabc...
cargo run -- employee-stats show --wallet 0xabc...
```

Refresh only current positions, or rebuild all metrics from cached raw data:

```bash
cargo run -- employee-stats refresh --wallet 0xabc... --only positions
cargo run -- employee-stats rebuild --wallet 0xabc...
```

The report separates whole-wallet, primary-domain, other-domain, unclassified, and specialty performance. The first specialty rule is `WORLD_CUP`, matching `fifwc-*`, `world-cup`, and `World Cup` markets. It includes fill and action counts, trade-size percentiles, settled and mark-to-market win rates, realized/unrealized/combined PnL, ROI, profit factor, drawdown, profit concentration, high-price entry dependence, suspected market making, current open losses, and stale losing positions. Every output shows the latest trade, latest settlement, position-check time, and history-completeness status.

For resolved Polymarket binary positions, the report guards against `realizedPnl=0` hiding a win/loss: when reported PnL is effectively zero but `curPrice` is resolved to `1` or `0`, it imputes PnL from `(curPrice - avgPrice) * shares`. This keeps cached rebuilds from undercounting World Cup-style resolved positions.

Local state is stored under `logs/employee-stats/` by default:

```text
employee-stats.sqlite3
<wallet>/latest.json
<wallet>/latest.md
<wallet>/snapshots/<timestamp>.json
```

See [docs/employee_stats_execution_plan.md](docs/employee_stats_execution_plan.md) for metric definitions and acceptance criteria.

## WeatherHK Small Auto-Copy MVP

The first auto-copy module is intentionally narrow: it only targets the `WeatherHK` wallet in `WEATHER` markets. It follows the small high-frequency behavior we discussed:

- do not skip just because the trade is small, frequent, or repeated in the same market
- skip source BUYs below 1U because copying them as a fixed 1U order over-amplifies tiny probes
- size our order from WeatherHK's actual trade notional
- cancel pending buy orders when WeatherHK sells the same market/outcome, or when source-position reconciliation shows WeatherHK no longer holds that asset
- keep strict per-trade, per-market, daily spend, and daily realized-loss caps
- persist positions, pending orders, processed source trades, and execution logs in a local state file
- use `/activity` for live user activity because `/trades` can miss or lag WeatherHK maker/high-frequency fills

Default amount tiers:

```text
WeatherHK < 10U      => copy 1U
10U <= WeatherHK <30 => copy 2U
30U <= WeatherHK <60 => copy 3U
60U <= WeatherHK <100 => copy 5U
100U <= WeatherHK <200 => copy 8U
WeatherHK >= 200U   => copy 10U
```

Configure local secrets with `.env`:

```bash
cp .env.example .env
```

Then fill at least these values in `.env` before live trading:

```dotenv
POLYMARKET_PRIVATE_KEY=
POLYMARKET_FUNDER_ADDRESS=
POLYMARKET_SIGNATURE_TYPE=2
POLYMARKET_CHAIN_ID=137
POLYMARKET_CLOB_HOST=https://clob.polymarket.com
POLYMARKET_EXECUTOR_RETRIES=2
POLYMARKET_EXECUTOR_RETRY_BACKOFF_SECONDS=0.4
POLYMARKET_FORCE_SELL_FLOOR_PRICE=0.001
```

`.env` is ignored by git. The watcher loads it at startup and passes those variables to the external executor command. Shell environment variables still win if the same key is already set. Use `SMART_WALLET_ENV_FILE=/path/to/file` to load a different env file.

The WeatherHK auto-copy caps can also live in `.env` via keys such as `WEATHERHK_AUTO_COPY_ENABLED`, `WEATHERHK_AUTO_COPY_MODE`, `WEATHERHK_AUTO_COPY_EXEC`, `WEATHERHK_MAX_MARKET_EXPOSURE_USD`, `WEATHERHK_MAX_DAILY_SPEND_USD`, `WEATHERHK_MAX_CHASE_PCT`, and `WEATHERHK_MAX_CHASE_DELTA`. Direct FOK buys use the percentage chase limit only; passive BUY prices still use the configured absolute delta cap to protect edge. CLI flags override these defaults when provided.

Dry-run the WeatherHK logic without placing orders:

```bash
cargo run -- watch \
  --employee '0x488c725253fc21c7a9ca812030dc2f6343f98c1c:WeatherHK:WEATHER:weather|temperature|hurricane|storm|rain|snow:1:1' \
  --weatherhk-auto-copy \
  --weatherhk-auto-copy-mode dry-run \
  --poll-seconds 1
```

Live mode delegates trading to an external executor command. The watcher sends a JSON request on stdin and expects a small JSON result on stdout:

```bash
cargo run -- watch \
  --employee '0x488c725253fc21c7a9ca812030dc2f6343f98c1c:WeatherHK:WEATHER:weather|temperature|hurricane|storm|rain|snow:1:1' \
  --weatherhk-auto-copy \
  --weatherhk-auto-copy-mode live-external \
  --weatherhk-auto-copy-exec './scripts/polymarket_executor.sh' \
  --weatherhk-max-market-exposure 50 \
  --weatherhk-max-daily-spend 200 \
  --weatherhk-max-daily-loss 50 \
  --weatherhk-max-chase-pct 0.15 \
  --weatherhk-passive-offset-pct 0.05 \
  --weatherhk-max-chase-delta 0.03 \
  --weatherhk-passive-offset 0.02 \
  --weatherhk-buy-take-enabled true \
  --weatherhk-min-buy-source-notional 1 \
  --weatherhk-skip-buy-price-at-or-above 0.98 \
  --weatherhk-skip-buy-price-at-or-below 0.005 \
  --weatherhk-min-sell-sync-notional 1 \
  --weatherhk-passive-ttl 0 \
  --poll-seconds 1
```

For a BUY request, the external executor should:

1. Check the current best ask for the requested asset.
2. If `take_enabled` is true and ask is at or below `direct_limit_price`, buy immediately.
3. Otherwise place a passive limit buy at `passive_limit_price`.
4. Return `filled`, `pending`, `skipped`, or `failed`.

For WeatherHK, `take_enabled` is normally true for small-copy mode. If the current best ask is cheaper than or equal to the percentage-based direct chase limit, the follower should buy instead of skipping a reachable price. If the ask is above the direct limit or unavailable, the executor places a passive post-only buy at the passive limit, where the absolute delta cap still applies.

WeatherHK BUYs below `WEATHERHK_MIN_BUY_SOURCE_NOTIONAL_USD` are skipped by default. BUYs above `WEATHERHK_SKIP_BUY_PRICE_AT_OR_ABOVE` are skipped by default. This avoids copying very high probability / very low edge trades, such as 98c-99c entries where the remaining payout is too thin for a follower; exactly 98c is not skipped by this rule. Low-price BUY filtering is disabled by default with `WEATHERHK_SKIP_BUY_PRICE_AT_OR_BELOW=0`; sub-1c source trades may still execute at the exchange's minimum supported order price, so market/exposure caps remain important.

Any WeatherHK SELL for the same market/outcome cancels matching pending BUY orders first and, if we hold a tracked position, triggers market-exit selling for that outcome regardless of the source SELL notional. `WEATHERHK_MIN_SELL_SYNC_NOTIONAL_USD` is retained only for backward-compatible configs and should be `0`.

Set `WEATHERHK_PASSIVE_TTL_SECONDS=0` to keep passive BUY orders open until a WeatherHK sell, source-position reconciliation, or another explicit risk event cancels them. Source-position reconciliation also clears our tracked position when WeatherHK no longer holds that outcome. Routine pending syncs do not send Telegram messages; only fills, cancels, failures, and partial-fill progress are reported.

For a forced WeatherHK SELL request, the executor should exit immediately with a FAK market sell and no `min_sell_price`/slippage guard; partial fills are acceptable and the remainder is cancelled. `POLYMARKET_FORCE_SELL_FLOOR_PRICE` defaults to `0.001` so forced exits can hit sub-cent bids when WeatherHK exits at extreme low prices. For `cancel` and `sync`, it should cancel or report the state of a prior pending order.

The external executor retries transient CLOB/network failures such as `Request exception`, SSL timeout, and connection reset. It does not retry deterministic rejects such as geoblock, insufficient balance/allowance, minimum-size failures, or unavailable matching liquidity.

Example executor response:

```json
{
  "status": "filled",
  "order_id": "clob-order-id",
  "filled_amount_usd": 2.0,
  "filled_size": 4.7619,
  "filled_price": 0.42,
  "realized_pnl_usd": null,
  "message": "filled at best ask"
}
```

Supported status values include `filled`, `pending`, `submitted`, `cancelled`, `skipped`, and `failed`. Pending BUY orders are synced periodically and cancelled when TTL expires, unless TTL is `0`, or when WeatherHK sells the same market/outcome.

The module still does not store private keys or sign orders. It is a watcher, risk gate, state tracker, and executor dispatcher.

## Next Build Step

The next useful module is a `PolymarketDataClient` adapter:

1. Pull candidates from leaderboard by category.
2. Pull wallet trades and activity.
3. Pull market metadata and order books.
4. Derive `WalletMetrics`.
5. Rank wallets with `score_wallet`.
6. Emit discovery signals only when a high-scoring wallet enters at a copyable price.
