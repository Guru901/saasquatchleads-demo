#!/usr/bin/env bash
set -euo pipefail

API_BASE="${API_BASE:-http://localhost:3000}"

warm_search() {
  local location="$1" category="$2" response search_id state attempt
  response="$(curl --fail --silent --show-error \
    -H 'content-type: application/json' \
    -d "$(python3 -c 'import json,sys; print(json.dumps({"location":sys.argv[1],"category":sys.argv[2]}))' "$location" "$category")" \
    "$API_BASE/api/searches")"
  search_id="$(python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])' <<<"$response")"
  printf 'Started %s / %s (search %s)\n' "$category" "$location" "$search_id"

  for attempt in $(seq 1 180); do
    state="$(curl --fail --silent --show-error "$API_BASE/api/searches/$search_id")"
    local status
    status="$(python3 -c 'import json,sys; print(json.load(sys.stdin)["status"])' <<<"$state")"
    if [[ "$status" == "done" ]]; then
      printf 'Finished %s / %s; backend cache warmed.\n' "$category" "$location"
      return 0
    fi
    if [[ "$status" == "failed" ]]; then
      printf 'Search failed: %s\n' "$state" >&2
      return 1
    fi
    sleep 1
  done
  printf 'Timed out waiting for search %s\n' "$search_id" >&2
  return 1
}

warm_search "Los Angeles, California, United States" "Software engineering companies"
warm_search "Jaipur, Telangana, India" "Restaurants"
warm_search "Berlin, Germany" "Gyms"
