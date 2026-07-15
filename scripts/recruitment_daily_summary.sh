#!/bin/zsh
set -eu

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT_DIR"

LOG_DIR="${RECRUITMENT_LOG_DIR:-logs/recruitment}"
SUMMARY_PATH="${RECRUITMENT_SUMMARY_PATH:-$LOG_DIR/recruitment-summary.md}"

exec node scripts/recruitment_summary.js \
  --dir "$LOG_DIR" \
  --hours "${RECRUIT_SUMMARY_HOURS:-24}" \
  --top "${RECRUIT_SUMMARY_TOP:-20}" \
  --min-runs "${RECRUIT_SUMMARY_MIN_RUNS:-1}" \
  --min-qualified "${RECRUIT_SUMMARY_MIN_QUALIFIED:-2}" \
  --min-score "${RECRUIT_SUMMARY_MIN_SCORE:-0}" \
  --output "$SUMMARY_PATH" \
  "$@"
