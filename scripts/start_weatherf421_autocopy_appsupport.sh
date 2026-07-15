#!/bin/zsh
set -eu

APP_HOME="/Users/will/Library/Application Support/smart-wallet-discovery"
cd "$APP_HOME"

export WEATHERHK_SOURCE_WALLET="0xf421705cbe3dd07db21ddd4a61eb8cce9386efce"
export WEATHERHK_SOURCE_NAME="OlympusHive"
export WEATHERHK_AUTO_COPY_MODE="dry-run"
export WEATHERHK_STATE_PATH="logs/weatherf421_autocopy_state.json"
export WEATHERHK_STRATEGY_CONFIG_PATH="config/autocopy_event_basket_strategy.json"
export WEATHERHK_SPECIALTY_KEYWORDS="weather,temperature,precipitation,hurricane,typhoon,cyclone,storm,rain,snow,wind"
export WEATHERHK_SMALL_BUY_FULL_COPY_ENABLED=false
export WEATHERHK_BLOCKED_POSITION_KEYS=""
export WEATHERHK_RECONCILE_MAX_SOURCE_DRAWDOWN_PCT=0.95

exec "$APP_HOME/smart-wallet-discovery" watch \
  --employee '0xf421705cbe3dd07db21ddd4a61eb8cce9386efce:OlympusHive:WEATHER:weather|temperature|precipitation|hurricane|typhoon|cyclone|storm|rain|snow|wind:1:1' \
  --weatherhk-auto-copy \
  --poll-seconds 1 \
  --trade-limit 100 \
  --heartbeat-seconds 3600 \
  --no-profiles
