# Upside backend (milestone 1)

Rust API using Ripress 2.5.1. The frontend contract is `POST /api/searches`, `GET /api/searches/:id`, and `GET /health`. Searches run as Tokio tasks. Search state is held in memory and is lost when the process restarts.

## Setup

```sh
DATABASE_URL='postgres://…' ALLOWED_ORIGIN='http://localhost:5173' UPSIDE_CONTACT='you@example.com' cargo run
```

`DATABASE_URL` is optional for local use. When present, the app creates the `cache(key text primary key, value jsonb, fetched_at timestamptz)` table. `CACHE_TTL_HOURS` sets the normal cache TTL (default 24 hours); the two supported demo searches use 168 hours so the warm script can seed a week-long cache. `PORT` defaults to 3000. `ALLOWED_ORIGIN` is a comma-separated exact-origin allowlist; it defaults to `http://localhost:5173` and never allows `*`. Set it in production to your Vercel origin and, when needed, the localhost development origin, for example `https://your-app.vercel.app,http://localhost:5173`. `UPSIDE_CONTACT` is required and must be a monitored email address; requests identify as `UpsideBot/0.1 (+mailto:<UPSIDE_CONTACT>)`. `NOMINATIM_URL` can point to an authorized Nominatim-compatible geocoder, and `UPSIDE_REFERER` optionally sets the HTTP Referer. `OVERPASS_URLS` optionally overrides the sequential comma-separated mirror list. `OVERPASS_FORCE_IPV4` defaults to `true` and binds outbound HTTP to `0.0.0.0`; set it to `false` to allow IPv6.

`POST /api/searches` is limited to 2 requests per minute and 6 per hour per client IP. `GET /api/places/suggest` is limited to 30 requests per minute per client IP. At most 3 searches run concurrently across the process; further starts receive HTTP 429 with `Busy, try again shortly`. Rate limits are in-memory and reset when the process restarts.

## Deployment

Build the backend with `cargo build --release` and the frontend with `cd frontend && pnpm build`; serve `frontend/dist` as a static site and run the Rust API as a server process. Set `ALLOWED_ORIGIN`, `UPSIDE_CONTACT`, and `DATABASE_URL` in the deployment environment. Use Postgres when cache persistence across restarts or API replicas is required; the in-process cache is local to one backend process.

### Docker

Build from the repository root so the image can include the offline snapshots:

```sh
docker build -f backend/Dockerfile -t saasquatchleads-backend:latest .
docker run --rm -p 3000:3000 \
  -e UPSIDE_CONTACT='you@example.com' \
  -e ALLOWED_ORIGIN='https://your-app.vercel.app,http://localhost:5173' \
  saasquatchleads-backend:latest
```

`DATABASE_URL` is optional; add it to `docker run` when using Postgres for persistent cache. The container runs as an unprivileged user and stores demo snapshots in `/app/demo_data`.

## Architecture

The frontend is React, TypeScript, and Vite. It loads category options from `GET /api/categories` and requests city/town/village typeahead from `GET /api/places/suggest?q=`; Photon suggestions are cached for 24 hours and the endpoint is rate-limited per forwarded client IP. Photon, geocoding, and bounded Nominatim requests send `Accept-Language: en`. Photon’s public demo server is best-effort and asks projects to keep requests reasonable; extensive usage may be throttled or banned, with no availability guarantee. A normal search-box typeahead is suitable; use a self-hosted instance for larger usage. See [Photon’s usage terms](https://github.com/komoot/photon#photon-api). It sends `POST /api/searches` and polls `GET /api/searches/:id`; the Rust/Tokio API does discovery and enrichment in background tasks. Search state lives in process memory, while cached geocoding and discovery results use a local in-memory TTL cache and optional Postgres persistence. Discovery follows cache → Overpass → bounded Nominatim → matching offline snapshot. Enrichment streams completed rows into the polled search state.

The frontend draft sender and neutral offer line can be set at build time with `DRAFT_SENDER_NAME` and `DRAFT_OFFER` in the frontend environment. For example, set these in `frontend/.env.production` before running `pnpm build`. The defaults are `Your name` and a neutral question about whether the topic is a priority. These values configure local draft text only; no send endpoint or API contract is added.

Example:

```sh
curl -X POST http://localhost:3000/api/searches -H 'content-type: application/json' -d '{"location":"Austin, Texas","category":"Restaurants"}'
curl http://localhost:3000/api/searches/SEARCH_ID
```

## Discovery and data use

Discovery order is cached Overpass or Nominatim results, live Overpass, bounded Nominatim fallback, then the matching offline snapshot. Country, state, and region-level geocoding results are rejected with HTTP 400; accepted results expose Nominatim's display name and bounding box. Overpass tries at most two configured mirrors, skips open circuit breakers, and gives each request up to 12 seconds within a 25-second budget. Mirrors are tried sequentially; each failed mirror is skipped for five minutes. Failures log endpoint details, while clients receive a friendly map-server message. Defaults are `overpass-api.de` and `lz4.overpass-api.de`; set `OVERPASS_URLS` to use a provider you operate or are authorized to use. Cache keys include category, center coordinates rounded to two decimals, and radius.

If Overpass fails, Nominatim searches inside the geocoded bounding box, converting `[south,north,west,east]` into the API's `viewbox=west,north,east,south`. It requests `jsonv2`, extra tags, address details, a maximum of 40 results, and the configured contact email. Bounded POI keywords live in `backend/src/nominatim_keywords.rs`; an empty or unrecognized keyword triggers a query for `<category> in <place>`. Nominatim results use the configured cache TTL. `SearchState.source` reports `live`, `nominatim`, `cache`, or `snapshot`. The footer shows `Data © OpenStreetMap contributors` and appends `· backup source` for non-live results.

The public Nominatim service is a limited community service, not a complete POI download API. Its usage policy requires no more than one request per second across the application, an identifying User-Agent or Referer, and clear attribution; systematic queries intended to download all POIs in an area are prohibited. The bounded search has a hard 40-result cap and may return fewer or incomplete matches. Use a self-hosted or authorized provider for systematic or commercial-scale lead discovery. See the [Nominatim Usage Policy](https://operations.osmfoundation.org/policies/nominatim/) and [Search API limits](https://nominatim.org/release-docs/latest/api/Search/).

## Demo fallback

Run `scripts/warm_demo.sh` against a running backend to warm Software engineering companies / Los Angeles, Restaurants / Jaipur, Telangana, and Gyms / Berlin searches. These searches use the normal cache TTL. The existing offline snapshots remain Auto repair / Jaipur and Restaurants / Berlin; only a successful live Overpass response with a valid `elements` array can update `demo_data/auto_repair_jaipur.json` or `demo_data/restaurants_berlin.json`. Cache and snapshot fallbacks never overwrite those files. A search state includes an optional `source`: `live`, `nominatim`, `cache`, or `snapshot`. The footer labels non-live results as a backup source.

Site enrichment runs up to 12 leads concurrently. Requests to a business host (robots.txt, homepage, and contact page) are spaced at about one request per second per hostname. Nominatim requests share a global one-request-per-second limiter; each Overpass mirror is paced at one request per second per hostname. Per-search logs include elapsed milliseconds for `robots_txt`, `homepage`, `contact_page`, and `mx` stages, including explicit skip outcomes when a stage does not run. Leads are added to search state as workers finish.

## Scoring

The score estimates digital-upgrade opportunities from available public data. A missing OSM website tag is scored as “not listed in OpenStreetMap (unverified),” not as proof the business lacks a website. Brand/chain detection is heuristic and franchise locations receive a penalty.

## Ethical data collection

The planned collection policy is to use OpenStreetMap and each business's own public website only. Do not scrape LinkedIn, Google, or sites whose terms forbid scraping. Identify the client with `USER_AGENT`, respect robots.txt, rate-limit requests, do not evade CAPTCHAs or blocks, and collect only publicly listed business contact details.

## Limitations

- Search state is volatile and single-process; no authentication is implemented.
- Postgres is a best-effort cache; without it, cache misses are not persisted between searches.
- Demo snapshots can become stale; use the warm script to refresh them when Overpass is available. When one is used, `SearchState.source` is `snapshot`; the footer marks non-live results as a backup source. The `source` field also reports `live`, `nominatim`, or `cache`; the optional `place` field contains the Nominatim display name. Public Nominatim may change or deny access under its usage policy and cannot provide complete business lists. Photon’s public demo server is best-effort and may throttle excessive autocomplete use.
