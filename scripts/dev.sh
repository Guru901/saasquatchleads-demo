#!/usr/bin/env bash
set -u

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BACKEND_DIR="$ROOT_DIR/backend"
backend_pid=""
frontend_pid=""

stop_backend() {
  if [[ -n "$backend_pid" ]] && kill -0 "$backend_pid" 2>/dev/null; then
    kill "$backend_pid" 2>/dev/null || true
    wait "$backend_pid" 2>/dev/null || true
  fi
  backend_pid=""
}

cleanup() {
  trap - INT TERM EXIT
  stop_backend
  if [[ -n "$frontend_pid" ]] && kill -0 "$frontend_pid" 2>/dev/null; then
    kill "$frontend_pid" 2>/dev/null || true
    wait "$frontend_pid" 2>/dev/null || true
  fi
}
trap cleanup INT TERM EXIT

# The backend requires a contact email for its public data requests.
export UPSIDE_CONTACT="${UPSIDE_CONTACT:-dev@example.com}"
export ALLOWED_ORIGIN="${ALLOWED_ORIGIN:-http://localhost:5173}"

(
  cd "$ROOT_DIR/frontend"
  exec pnpm run dev
) &
frontend_pid=$!

start_backend() {
  echo "Starting backend (http://localhost:${PORT:-3000})"
  cargo run --manifest-path "$BACKEND_DIR/Cargo.toml" &
  backend_pid=$!
}

backend_fingerprint() {
  find "$BACKEND_DIR/src" -type f -print0 | sort -z | xargs -0 -r cksum
  cksum "$BACKEND_DIR/Cargo.toml" "$BACKEND_DIR/Cargo.lock"
}

start_backend
previous_fingerprint="$(backend_fingerprint)"

while true; do
  sleep 1
  current_fingerprint="$(backend_fingerprint)"
  if [[ "$current_fingerprint" != "$previous_fingerprint" ]]; then
    echo "Backend files changed; restarting..."
    stop_backend
    start_backend
    previous_fingerprint="$current_fingerprint"
  elif ! kill -0 "$backend_pid" 2>/dev/null; then
    echo "Backend stopped; restarting..."
    start_backend
  fi

  if ! kill -0 "$frontend_pid" 2>/dev/null; then
    echo "Frontend dev server stopped."
    exit 1
  fi
done
