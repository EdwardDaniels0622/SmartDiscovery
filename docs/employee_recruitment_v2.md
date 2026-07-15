# Employee Recruitment V2

## Background

The old employee scan starts from Polymarket leaderboards. That finds wallets that made money, but it does not reliably find wallets that a small account can copy. Recent observation shows the problem clearly:

- `WeatherHK` is usable because it is narrow, active, small enough, and leaves observable buy/sell rhythm.
- `HighTempTation` may be profitable, but many entries look like late certainty buying after the outcome is mostly known. That is not a good small-copy employee.
- Most leaderboard employees produce few actionable alerts.

The new recruitment flow starts from trades, not rankings. The core question changes from "who made money?" to "who repeatedly buys before the market tape moves, while still leaving a copyable window?"

## Goals

- Discover candidate employees from global market tape, without relying on leaderboards as the primary source.
- Score individual BUY trades by small-copy suitability and post-trade price movement.
- Aggregate good trades back to wallet + domain candidates.
- Output ready-to-watch employee specs for the existing watcher.
- Keep the first implementation read-only and inspectable.

## Non-Goals

- No automatic trading.
- No private keys or signing.
- No full order-book simulation in the first version.
- No claim that tape movement is final realized PnL; it is a recruitment signal.

## Candidate Source

Version 2 uses the public Data API `/trades` endpoint without a `user` parameter to pull recent global trades. This creates a rolling tape window.

The scanner should collect several pages of recent trades, then evaluate BUY trades against later trades for the same `conditionId + asset` inside the same window.

## Good Trade Definition

A trade is a good recruitment signal when:

- It is a BUY.
- Entry price is in a copyable range, default `1c-75c`.
- Source notional is meaningful but not whale-only, default `$5-$1,000`.
- Market text matches a configured domain keyword set.
- The market is not an excluded ultra-fast or late-certainty pattern by default, such as "Up or Down" 5-minute crypto markets.
- Later tape for the same outcome exists inside the lookahead window.
- Later price moves in the same direction by at least the configured threshold, default `1.5c`.
- The move does not happen only instantly; there should be a minimum copy window, default `20s`.

## Candidate Employee Definition

A candidate employee is a wallet + domain pair with repeated good trades.

Default minimums:

- At least 2 qualified good trades in the sampled tape.
- Candidate score at least 60.
- Positive post-trade movement rate should be visible in the output.

The candidate output should include:

- domain
- wallet
- name or pseudonym
- candidate score
- qualified trades / evaluated trades
- positive movement rate
- average entry price
- average post-trade move
- median source notional
- last seen timestamp
- ready-to-use watcher spec
- wallet-health summary, when live vetting is enabled
- reasons
- cautions
- top example trades

Live recruitment treats tape hits as leads, not hires. Before a lead is promoted to the candidate list, the scanner vets the wallet with sampled closed positions and current positions. The default hard filters reject wallets that have too little closed-position history, negative realized PnL/ROI, negative recent PnL/ROI when enough recent samples exist, current/open loss above `$5,000`, current/open loss ratio above `20%`, a high same-market two-sided footprint, or an explicit recruitment reject-list match.

## Scoring

Trade quality score should combine:

- post-trade move size
- entry price copyability
- source notional suitability
- available copy window

Candidate score should combine:

- average qualified trade quality
- positive move rate across evaluated trades
- repeated-signal count

## Employee Layers

Recruitment output does not immediately make every wallet a core employee. Candidates should be interpreted as:

- Core candidate: repeated qualified signals, good copy window, small/medium notional, strong score.
- Opportunity candidate: low sample but strong example trades.
- Watch-only: informative but too late, too large, or too fast to copy.
- Reject: late certainty, noisy, market-making, or no repeatability.

## First-Version Limits

The first version uses later trades in the sampled tape as a proxy for price movement. This is weaker than true order-book replay. The next improvements should be:

- Pull current order book for top candidate trades.
- Add delayed-copy backtest with 30s, 2m, and 5m entry assumptions.
- For leaderboard-first CRYPTO recruitment, exclude 5/10/15-minute markets from ordinary employee PnL, ROI, and sample counts. Keep their position/profit/capital shares as observation-only diagnostics until delayed-copy testing exists.
- Track candidates over multiple days in local storage.
- Promote/demote candidates based on rolling score, not one scan.
- Add domain-specific exclusions and market pools.
- Add persisted promotion states so rejected leads, trial leads, and core employees are tracked separately across days.

## CLI

Run:

```bash
cargo run -- recruit-employees --top 10 --trade-pages 5
```

Useful flags:

```bash
cargo run -- recruit-employees --domains WEATHER,TECH --trade-pages 10
cargo run -- recruit-employees --min-tape-move 0.02 --min-candidate-trades 3
cargo run -- recruit-employees --max-current-loss 2500 --reject-wallets 0xabc,0xdef
cargo run -- recruit-employees --include-fast-markets
cargo run -- recruit-employees --json
```
