#!/bin/zsh
set -eu

APP_HOME="/Users/will/Desktop/freedom/smart-wallet-discovery"
cd "$APP_HOME"

exec "$APP_HOME/target/debug/smart-wallet-discovery" watch \
  --employee '0x488c725253fc21c7a9ca812030dc2f6343f98c1c:WeatherHK:WEATHER:weather|temperature|hurricane|storm|rain|snow:5:1' \
  --weatherhk-auto-copy \
  --poll-seconds 1 \
  --trade-limit 100 \
  --heartbeat-seconds 3600 \
  --no-profiles
