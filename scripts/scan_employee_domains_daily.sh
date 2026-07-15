#!/bin/zsh
set -eu

export PATH="$HOME/.cargo/bin:$HOME/.local/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT_DIR"

DOMAINS=(WEATHER CRYPTO SPORTS)
LOG_DIR="${EMPLOYEE_SCAN_LOG_DIR:-logs/employee-discovery}"
STATE_PATH="${EMPLOYEE_SCAN_STATE_PATH:-$LOG_DIR/rotation-state-v3}"
REPORT_PATH="${EMPLOYEE_SCAN_REPORT_PATH:-$LOG_DIR/employee-discovery-v3.md}"
LOCK_DIR="$LOG_DIR/.daily-scan.lock"
PROXY_URL="${POLYMARKET_PROXY_URL:-http://127.0.0.1:7890}"
PROXY_WAIT_ATTEMPTS="${EMPLOYEE_SCAN_PROXY_WAIT_ATTEMPTS:-10}"
PROXY_WAIT_SECONDS="${EMPLOYEE_SCAN_PROXY_WAIT_SECONDS:-30}"

mkdir -p "$LOG_DIR"
if ! mkdir "$LOCK_DIR" 2>/dev/null; then
  echo "employee domain scan is already running"
  exit 0
fi
trap 'rmdir "$LOCK_DIR" 2>/dev/null || true' EXIT

INDEX=0
if [[ -f "$STATE_PATH" ]]; then
  INDEX="$(tr -cd '0-9' < "$STATE_PATH")"
  INDEX="${INDEX:-0}"
fi

if [[ -n "${EMPLOYEE_SCAN_DOMAIN:-}" ]]; then
  DOMAIN="${EMPLOYEE_SCAN_DOMAIN:u}"
  if (( ${DOMAINS[(Ie)$DOMAIN]} == 0 )); then
    echo "unsupported EMPLOYEE_SCAN_DOMAIN=$DOMAIN"
    exit 1
  fi
else
  DOMAIN="${DOMAINS[$((INDEX % ${#DOMAINS[@]} + 1))]}"
fi

RUN_ID="$(date '+%Y-%m-%d_%H-%M-%S')"
JSON_PATH="$LOG_DIR/$RUN_ID-$DOMAIN.json"
TMP_PATH="$LOG_DIR/.$RUN_ID-$DOMAIN.tmp"
FAILED_DIR="$LOG_DIR/failures"

if [[ -n "${SMART_WALLET_BINARY:-}" ]]; then
  RUNNER=("$SMART_WALLET_BINARY")
elif [[ -x "$ROOT_DIR/employee-discovery" ]]; then
  RUNNER=("$ROOT_DIR/employee-discovery")
elif [[ -x "$ROOT_DIR/smart-wallet-discovery" ]]; then
  RUNNER=("$ROOT_DIR/smart-wallet-discovery")
else
  RUNNER=(cargo run --quiet --)
fi

proxy_ready() {
  curl --proxy "$PROXY_URL" \
    --silent \
    --fail \
    --connect-timeout 4 \
    --max-time 8 \
    --get "https://data-api.polymarket.com/v1/leaderboard" \
    --data-urlencode "category=$DOMAIN" \
    --data-urlencode "timePeriod=MONTH" \
    --data-urlencode "orderBy=PNL" \
    --data-urlencode "limit=1" \
    --data-urlencode "offset=0" >/dev/null 2>&1
}

ATTEMPT=1
while ! proxy_ready; do
  if (( ATTEMPT >= PROXY_WAIT_ATTEMPTS )); then
    echo "Polymarket proxy is unavailable after $ATTEMPT attempts: $PROXY_URL"
    exit 1
  fi
  echo "waiting for Polymarket proxy ($ATTEMPT/$PROXY_WAIT_ATTEMPTS)"
  sleep "$PROXY_WAIT_SECONDS"
  ATTEMPT=$((ATTEMPT + 1))
done

POLYMARKET_PROXY_URL="$PROXY_URL" "${RUNNER[@]}" scan-domain-employees \
  --domain "$DOMAIN" \
  --periods "${EMPLOYEE_SCAN_PERIODS:-DAY,WEEK,MONTH,ALL}" \
  --leaderboard-depth "${EMPLOYEE_SCAN_LEADERBOARD_DEPTH:-1000}" \
  --wallet-limit "${EMPLOYEE_SCAN_WALLET_LIMIT:-1000}" \
  --history-days "${EMPLOYEE_SCAN_HISTORY_DAYS:-30}" \
  --closed-pages "${EMPLOYEE_SCAN_CLOSED_PAGES:-20}" \
  --pause-ms "${EMPLOYEE_SCAN_PAUSE_MS:-120}" \
  --top "${EMPLOYEE_SCAN_TOP:-20}" \
  --min-lifetime-pnl "${EMPLOYEE_SCAN_MIN_LIFETIME_PNL:-15000}" \
  --max-lifetime-pnl "${EMPLOYEE_SCAN_MAX_LIFETIME_PNL:-400000}" \
  --min-monthly-positions "${EMPLOYEE_SCAN_MIN_MONTHLY_POSITIONS:-30}" \
  --max-monthly-positions "${EMPLOYEE_SCAN_MAX_MONTHLY_POSITIONS:-300}" \
  --json > "$TMP_PATH"

if ! node -e '
  const report = JSON.parse(require("fs").readFileSync(process.argv[1], "utf8"));
  const target = Math.min(report.pool_wallets, report.wallet_limit);
  const minimum = Math.max(1, Math.floor(target * 0.8));
  if (report.pool_wallets < 1 || report.scanned_wallets < minimum) process.exit(1);
' "$TMP_PATH"; then
  mkdir -p "$FAILED_DIR"
  mv "$TMP_PATH" "$FAILED_DIR/$RUN_ID-$DOMAIN.json"
  echo "scan did not complete enough wallets; rotation will retry $DOMAIN next run"
  exit 1
fi

mv "$TMP_PATH" "$JSON_PATH"
node scripts/render_domain_scan_report.js --input "$JSON_PATH" --output "$REPORT_PATH"

if [[ -z "${EMPLOYEE_SCAN_DOMAIN:-}" ]]; then
  NEXT_INDEX=$(((INDEX + 1) % ${#DOMAINS[@]}))
  printf '%s\n' "$NEXT_INDEX" > "$STATE_PATH.tmp"
  mv "$STATE_PATH.tmp" "$STATE_PATH"
fi

echo "domain=$DOMAIN json=$JSON_PATH report=$REPORT_PATH"
