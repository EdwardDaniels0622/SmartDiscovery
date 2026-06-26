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

Each market decision records the latest completed Binance 5m candle, the lookback candle used for the 30m return, UP/DOWN token ids, both UP and DOWN best asks, seconds after open, seconds before close, previous result, and observed previous-result latency. Separate `previous_result_observed` events are written the first time the script sees a completed previous market result.

## Safety Defaults

- `mode=paper`
- `enabled=false`
- `trade_down=true`
- `trade_up=false`
- fixed stake only, no martingale
- one decision per 5m market
- no new entry inside the last 90 seconds
- skip entries above `max_entry_price`, with `hard_max_entry_price` as an absolute guard
- pause after two consecutive losses until the Binance 30m trend turns

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
