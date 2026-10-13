#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

(
  cd "$ROOT_DIR/frontend"
  VITE_API_URL="${VITE_API_URL:-http://localhost:3000}" pnpm run build
)

cargo build --release --manifest-path "$ROOT_DIR/backend/Cargo.toml"
