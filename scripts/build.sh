#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if command -v pnpm >/dev/null 2>&1; then
  FRONTEND_PM=pnpm
elif command -v npm >/dev/null 2>&1; then
  FRONTEND_PM=npm
else
  echo "Neither pnpm nor npm is installed; one is required to build the frontend." >&2
  exit 1
fi

(
  cd "$ROOT_DIR/frontend"
  VITE_API_URL="${VITE_API_URL:-http://localhost:3000}" "$FRONTEND_PM" run build
)

cargo build --release --manifest-path "$ROOT_DIR/backend/Cargo.toml"
