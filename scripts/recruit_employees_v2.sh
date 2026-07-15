#!/bin/zsh
set -eu

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT_DIR"

exec cargo run -- recruit-employees "$@"
