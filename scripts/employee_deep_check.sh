#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ACTION="${1:-}"
EMPLOYEE="${2:-}"

if [[ "$ACTION" != "show" && "$ACTION" != "refresh" && "$ACTION" != "rebuild" ]]; then
  echo "usage: $0 <show|refresh|rebuild> <wallet|cached-alias|employee-spec> [employee-stats options...]" >&2
  exit 2
fi

if [[ -z "$EMPLOYEE" ]]; then
  echo "employee wallet, cached alias, or employee spec is required" >&2
  exit 2
fi

shift 2
ARGS=(employee-stats "$ACTION")

if [[ "$EMPLOYEE" == *:* || "$EMPLOYEE" != 0x* ]]; then
  ARGS+=(--employee "$EMPLOYEE")
else
  ARGS+=(--wallet "$EMPLOYEE")
fi

cd "$ROOT_DIR"

if [[ -n "${SMART_WALLET_DISCOVERY_BINARY:-}" ]]; then
  exec "$SMART_WALLET_DISCOVERY_BINARY" "${ARGS[@]}" "$@"
fi

exec cargo run --quiet -- "${ARGS[@]}" "$@"
