#!/bin/zsh
set -eu

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT_DIR"

export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"

LOG_DIR="${RECRUITMENT_LOG_DIR:-logs/recruitment}"
mkdir -p "$LOG_DIR"
SUMMARY_PATH="${RECRUITMENT_SUMMARY_PATH:-$LOG_DIR/recruitment-summary.md}"

STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
JSON_PATH="$LOG_DIR/recruitment-$STAMP.json"

ARGS=(
  --top "${RECRUIT_TOP:-50}"
  --trade-pages "${RECRUIT_TRADE_PAGES:-20}"
  --min-candidate-score "${RECRUIT_MIN_CANDIDATE_SCORE:-0}"
  --min-candidate-trades "${RECRUIT_MIN_CANDIDATE_TRADES:-1}"
  --min-tape-move "${RECRUIT_MIN_TAPE_MOVE:-0.015}"
)

if [[ -n "${RECRUIT_DOMAINS:-}" ]]; then
  ARGS+=(--domains "$RECRUIT_DOMAINS")
fi

if [[ "${RECRUIT_INCLUDE_FAST_MARKETS:-0}" == "1" ]]; then
  ARGS+=(--include-fast-markets)
fi

if [[ -x "$ROOT_DIR/smart-wallet-discovery" ]]; then
  "$ROOT_DIR/smart-wallet-discovery" recruit-employees "${ARGS[@]}" --json "$@" > "$JSON_PATH"
else
  cargo run --quiet -- recruit-employees "${ARGS[@]}" --json "$@" > "$JSON_PATH"
fi

echo "saved: $JSON_PATH"
node scripts/recruitment_summary.js \
  --dir "$LOG_DIR" \
  --hours "${RECRUIT_ROLLUP_HOURS:-24}" \
  --top "${RECRUIT_ROLLUP_TOP:-20}" \
  --min-qualified "${RECRUIT_ROLLUP_MIN_QUALIFIED:-1}" \
  --min-score "${RECRUIT_ROLLUP_MIN_SCORE:-0}" \
  --output "$SUMMARY_PATH"

echo "summary: $SUMMARY_PATH"
