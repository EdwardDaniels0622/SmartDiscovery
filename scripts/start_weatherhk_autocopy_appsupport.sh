#!/bin/zsh
set -eu

APP_HOME="/Users/will/Library/Application Support/smart-wallet-discovery"
cd "$APP_HOME"
export WEATHERHK_BLOCKED_POSITION_KEYS='0x2be41477e2769fadb31a361825e87d03c329871314ed6a20aa5466ce0f39d382:53990757514422420421295083379039912533281881996757480163569472850688496817153'
export WEATHERHK_RECONCILE_MAX_SOURCE_DRAWDOWN_PCT=0.95

exec "$APP_HOME/smart-wallet-discovery" watch \
  --employee '0x488c725253fc21c7a9ca812030dc2f6343f98c1c:WeatherHK:WEATHER:weather|temperature|precipitation|hurricane|typhoon|cyclone|storm|rain|snow|wind:1:1' \
  --weatherhk-auto-copy \
  --poll-seconds 1 \
  --trade-limit 100 \
  --heartbeat-seconds 3600 \
  --no-profiles
