#!/usr/bin/env sh
set -eu

APP_ROOT="/Users/will/Library/Application Support/smart-wallet-discovery"
PYTHON="${APP_ROOT}/.venv/bin/python"
SCRIPT="${APP_ROOT}/scripts/btc_5m_follow.py"
CONFIG="${APP_ROOT}/config/btc_5m_follow.paper.json"

if [ ! -x "$PYTHON" ]; then
  PYTHON="/usr/bin/python3"
fi

mkdir -p "$APP_ROOT/logs" "$APP_ROOT/state" "$APP_ROOT/.pycache"
export PYTHONPYCACHEPREFIX="$APP_ROOT/.pycache"

exec "$PYTHON" "$SCRIPT" --config "$CONFIG"
