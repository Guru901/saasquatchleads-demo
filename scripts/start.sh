#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
FRONTEND_DIR="$ROOT_DIR/frontend"
BACKEND_BIN="$ROOT_DIR/backend/target/release/backend"
frontend_pid=""
backend_pid=""

if [[ ! -d "$FRONTEND_DIR/dist" || ! -x "$BACKEND_BIN" ]]; then
  echo "Build artifacts are missing. Run ./scripts/build.sh first." >&2
  exit 1
fi

export UPSIDE_CONTACT="${UPSIDE_CONTACT:-dev@example.com}"
export ALLOWED_ORIGIN="${ALLOWED_ORIGIN:-http://localhost:4173}"

cleanup() {
  trap - INT TERM EXIT
  for pid in "$frontend_pid" "$backend_pid"; do
    if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
      kill "$pid" 2>/dev/null || true
      wait "$pid" 2>/dev/null || true
    fi
  done
}
trap cleanup INT TERM EXIT

(
  cd "$FRONTEND_DIR"
  if command -v pnpm >/dev/null 2>&1; then
    exec pnpm run preview -- --host 127.0.0.1 --port "${FRONTEND_PORT:-4173}" --strictPort
  else
    exec npm run preview -- --host 127.0.0.1 --port "${FRONTEND_PORT:-4173}" --strictPort
  fi
) &
frontend_pid=$!
"$BACKEND_BIN" &
backend_pid=$!

echo "Frontend: http://localhost:${FRONTEND_PORT:-4173}"
echo "Backend:  http://localhost:${PORT:-3000}"

wait -n "$frontend_pid" "$backend_pid"
