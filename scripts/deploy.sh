#!/usr/bin/env bash
set -Eeuo pipefail

ROOT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
FRONTEND_DIR="$ROOT_DIR/frontend"
BACKEND_DOCKERFILE="$ROOT_DIR/backend/Dockerfile"
BACKEND_LOCAL_IMAGE="saasquatchleads-backend:latest"
BACKEND_IMAGE="${DOCKER_IMAGE:-guru911u/saasquatchleads-backend}"

usage() {
  cat <<'EOF'
Usage: scripts/deploy.sh [frontend|backend]

  frontend  Deploy the frontend to Vercel production.
  backend   Build and push the backend Docker image to Docker Hub as :latest.
  no arg    Run both (backend first, then frontend).

Optional environment variables:
  DOCKER_IMAGE         Docker Hub image name (default: guru911u/saasquatchleads-backend)
  DOCKERHUB_USERNAME   Docker Hub username; pair with DOCKERHUB_TOKEN to log in
  DOCKERHUB_TOKEN      Docker Hub access token
  DOCKER_USE_SUDO=1    Run Docker commands with sudo (optional; uses root's Docker login)
  VERCEL_TOKEN         Vercel CLI token; otherwise uses local Vercel CLI login
  VERCEL_ORG_ID        Vercel team/org ID for non-interactive deployments
  VERCEL_PROJECT_ID    Vercel project ID for non-interactive deployments

If Docker Hub credentials are not provided, run 'docker login' before deploying.
EOF
}

die() {
  printf 'deploy: %s\n' "$1" >&2
  exit 1
}

deploy_backend() {
  local -a docker_cmd=(docker)
  if [[ "${DOCKER_USE_SUDO:-0}" == "1" ]]; then
    command -v sudo >/dev/null 2>&1 || die "DOCKER_USE_SUDO=1 was set, but sudo is not installed."
    docker_cmd=(sudo docker)
  fi

  command -v "${docker_cmd[0]}" >/dev/null 2>&1 || die "Docker CLI not found. Install Docker and make sure its daemon is running."
  [[ -f "$BACKEND_DOCKERFILE" ]] || die "Missing backend Dockerfile at backend/Dockerfile."

  if [[ -n "${DOCKERHUB_USERNAME:-}" || -n "${DOCKERHUB_TOKEN:-}" ]]; then
    [[ -n "${DOCKERHUB_USERNAME:-}" && -n "${DOCKERHUB_TOKEN:-}" ]] \
      || die "Set both DOCKERHUB_USERNAME and DOCKERHUB_TOKEN, or log in with 'docker login' first."
    printf '%s' "$DOCKERHUB_TOKEN" | "${docker_cmd[@]}" login --username "$DOCKERHUB_USERNAME" --password-stdin
  else
    printf "No Docker Hub credentials supplied. If needed, run '%s login' before deploying.\n" "${docker_cmd[*]}"
  fi

  printf 'Building %s…\n' "$BACKEND_LOCAL_IMAGE"
  "${docker_cmd[@]}" build \
    --file "$BACKEND_DOCKERFILE" \
    --tag "$BACKEND_LOCAL_IMAGE" \
    "$ROOT_DIR"
  printf 'Tagging as %s:latest…\n' "$BACKEND_IMAGE"
  "${docker_cmd[@]}" tag "$BACKEND_LOCAL_IMAGE" "$BACKEND_IMAGE:latest"
  printf 'Pushing %s:latest to Docker Hub…\n' "$BACKEND_IMAGE"
  "${docker_cmd[@]}" push "$BACKEND_IMAGE:latest"
  printf 'Pushed %s:latest\n' "$BACKEND_IMAGE"
}

deploy_frontend() {
  command -v pnpm >/dev/null 2>&1 || die "pnpm is required to run the Vercel CLI."
  [[ -f "$FRONTEND_DIR/.env" ]] || die "Missing frontend/.env; add VITE_API_URL before deploying."
  [[ -f "$BACKEND_DOCKERFILE" ]] || die "Missing backend Dockerfile at backend/Dockerfile."

  local api_url
  api_url="$(sed -n 's/^VITE_API_URL=//p' "$FRONTEND_DIR/.env" | head -n 1)"
  [[ -n "$api_url" ]] || die "frontend/.env must define VITE_API_URL."
  [[ "$api_url" == https://* ]] || die "VITE_API_URL must be an HTTPS URL for production."

  if [[ ! -f "$FRONTEND_DIR/.vercel/project.json" \
    && ( -z "${VERCEL_ORG_ID:-}" || -z "${VERCEL_PROJECT_ID:-}" ) ]]; then
    die "Link frontend/ to the existing Vercel project once with 'pnpm dlx vercel link', or set VERCEL_ORG_ID and VERCEL_PROJECT_ID."
  fi

  printf 'Deploying frontend to Vercel production (API: %s)…\n' "$api_url"
  cd "$FRONTEND_DIR"
  if [[ -n "${VERCEL_TOKEN:-}" ]]; then
    pnpm dlx vercel deploy --prod --yes --token "$VERCEL_TOKEN" --build-env "VITE_API_URL=$api_url"
  else
    pnpm dlx vercel deploy --prod --yes --build-env "VITE_API_URL=$api_url"
  fi
}

target="${1:-both}"
[[ $# -le 1 ]] || { usage >&2; exit 2; }

case "$target" in
  frontend|front)
    deploy_frontend
    ;;
  backend|back)
    deploy_backend
    ;;
  both)
    deploy_backend
    deploy_frontend
    ;;
  -h|--help|help)
    usage
    ;;
  *)
    usage >&2
    exit 2
    ;;
esac
