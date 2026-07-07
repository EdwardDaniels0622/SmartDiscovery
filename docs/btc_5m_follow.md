# BTC 5m Polymarket Follow Script

This is a low-frequency BTC 5m UP/DOWN follower in paper-first mode. It keeps the implementation separate from the existing Rust watcher and only reuses the current Polymarket executor when live mode is explicitly enabled.

## Quick Start

```sh
cp config/btc_5m_follow.example.json config/btc_5m_follow.json
python3 scripts/btc_5m_follow.py --config config/btc_5m_follow.json --once
python3 scripts/btc_5m_follow.py --config config/btc_5m_follow.json
```

Default behavior is paper mode. It will write state to `state/btc_5m_follow_state.json` and JSONL logs to `logs/btc_5m_follow_decisions.jsonl`.

The example config uses `http://127.0.0.1:7890` for both Binance and Polymarket. Set the proxy fields to `null` or `direct` if the machine can reach those APIs directly.

Each market decision records the latest completed Binance 5m candle, the lookback candle used for the 30m return, live Binance BTC price, UP/DOWN token ids, both UP and DOWN best asks, seconds after open, seconds before close, previous result, result source, target prices, target-price delta, and observed previous-result latency. Separate `previous_result_observed` events are written the first time the script sees a completed previous market result.

## Target-Price Result Mode

Polymarket official settlement can arrive too late for a 5m market, so the script now uses target and close prices as an early result signal. The primary metadata path is Gamma `eventMetadata`; when that is missing, the script fetches the Polymarket event page and reads the page's own `crypto-prices` / `past-results` data from either legacy `__NEXT_DATA__` or current Next App Router flight data.

```text
preferred previous result = previous final/close price - previous priceToBeat
fallback previous result = current priceToBeat - previous priceToBeat
```

If the delta is positive, the previous market is treated as `UP`; if negative, it is treated as `DOWN`. Official Polymarket results still take priority when available. When official settlement is not yet available, decisions use `previous_result_source=computed_price_to_beat` and log:

- `previous_price_to_beat`
- `previous_final_price`
- `current_price_to_beat`
- `computed_price_delta`
- `computed_previous_result`
- `official_previous_result`
- `previous_result_source`
- `current_page_price_source`
- `previous_page_price_source`
- `binance_live_price`
- `current_price_delta_to_target`
- `current_price_side`

The script also stores computed results in state and periodically checks them against official settlement. Each completed check writes a `computed_result_verified` JSONL event with `match=true/false`, so target-price accuracy can be audited over time. Paper settlement now prefers computed target-price results and later verifies them against official settlement.

## Shadow Strategies

The main paper strategy is selected with `main_strategy`. The current research deployment uses `anti_previous_result_v12` as the main paper strategy:

- `anti_previous_result_v12`: previous-result mean reversion; buy the opposite side of the previous 5m result, require confirmation latency at or below 60 seconds, enter within the first 90 seconds, and require entry price above `0.50` and at or below `0.70`

Decisions include a `shadow_strategies` array, and simulated entries are settled with `shadow_settlement` JSONL events. These strategies do not call the live executor and do not affect the main paper account. Current built-ins are:

- `base_v1`: trend plus previous same-direction result, max price `0.51`
- `delta_filter_v2`: `base_v1`, skip target-price deltas below `$5`, skip entry prices `0.31` through `0.40`
- `price60_v3`: trend plus previous same-direction result, max price `0.60`
- `current_side_price60_v4`: `price60_v3`, additionally require live BTC price to be on the selected side of the target price
- `fast_c_v5`: trend plus previous same-direction result, confirmation latency at or below 25 seconds, entry price above `0.35` and at or below `0.70`
- `anti_previous_result_v6`: previous-result mean reversion; buy the opposite side of the previous 5m result, require confirmation latency at or below 60 seconds, entry within the first 60 seconds, and entry price above `0.40` and at or below `0.70`
- `anti_previous_result_v7`: v6 with a tighter entry-price window, above `0.45` and at or below `0.60`
- `anti_previous_result_v8`: v7 plus a confirmation-latency window; previous-result latency must be above 30 seconds and at or below 60 seconds
- `last_minute_favorite_v13`: during the final 60 seconds, buy the side with the higher ask if that ask is above `0.70` and at or below `0.98`; this does not require the previous result

When the main paper decision has already been recorded before the previous result becomes known, the script writes a separate `research_observation` event as soon as that result is available. This preserves late-confirmation samples, including confirmations after 30 seconds, for shadow strategies and offline analysis without letting them affect the main paper account. The script also writes one `late_window_observation` per market inside the final 60 seconds so last-minute shadow strategies can be evaluated.

The `anti_previous_result_v6` candidate was selected from the 2026-06-27 through 2026-07-01 paper logs using chronological replay. In that sample it produced 389 simulated entries, 248 wins / 141 losses, a 63.75% win rate, and +63.39 USDC paper PnL at 1 USDC stake. The rule was positive on each logged calendar day in the sample, but remains a shadow strategy until forward paper data confirms it out of sample.

The `anti_previous_result_v7` and `anti_previous_result_v8` variants were added after the first forward paper run showed v6 was roughly flat and that narrower price and latency buckets had better forward paper results. They should be treated as live-forward research candidates, not as validated trading strategies.

## Launchd Paper Deployment

The LaunchAgent runs from:

```text
/Users/will/Library/Application Support/smart-wallet-discovery/start_btc_5m_follow_paper.sh
```

Paper state and decision data live under:

```text
/Users/will/Library/Application Support/smart-wallet-discovery/state/btc_5m_follow_state.json
/Users/will/Library/Application Support/smart-wallet-discovery/logs/btc_5m_follow_decisions.jsonl
```

Launchd stdout/stderr live under:

```text
/Users/will/Library/Logs/smart-wallet-discovery/btc5m-follow-paper.log
/Users/will/Library/Logs/smart-wallet-discovery/btc5m-follow-paper.err
```

## Safety Defaults

- `mode=paper`
- `enabled=false`
- `fixed_amount_usdc=1`
- `trade_down=true`
- `trade_up=true`
- current V12 paper test: `main_strategy=anti_previous_result_v12`, `min_entry_price=0.50`, `max_entry_price=0.70`, `max_previous_result_latency_seconds=60`
- fixed stake only, no martingale
- one decision per 5m market
- no entry before 5 seconds after market open
- no new entry inside the last 90 seconds
- skip entries above `max_entry_price`, with `hard_max_entry_price` as an absolute guard
- research deployments can raise `max_consecutive_losses` and daily limits to avoid suppressing paper samples

## Live Mode

Live mode calls `scripts/polymarket_executor.sh`, so the existing `.env` CLOB credentials and executor settings are reused.

To enable live trading, set both:

```json
{
  "mode": "live",
  "enabled": true
}
```

Keep the stake small until paper logs confirm the market lookup, previous-result delay, and order-book prices behave as expected.
