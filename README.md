# Upside

Upside is a local business lead finder. Search by place and business category, review public business details and digital presence signals, shortlist results, and export them as CSV. Discovery uses OpenStreetMap data and enrichment checks businesses' public websites. Outreach drafts can be edited and copied from a lead detail view; the app does not send messages.

## Requirements

- Rust toolchain with Cargo
- Node.js and pnpm (npm also works for the frontend)
- Optional: PostgreSQL, for cache persistence between backend restarts
- Docker to deploy the backend container

## Run locally

1. Configure the backend:

   ```sh
   cp backend/.env.example backend/.env
   ```

   Set `UPSIDE_CONTACT` to a monitored email address. `DATABASE_URL` is optional. The backend defaults to port `3000` and allows the Vite origin at `http://localhost:5173`.

2. Start the API in one terminal:

   ```sh
   cd backend
   set -a && source .env && set +a
   cargo run
   ```

3. Configure and start the frontend in another terminal:

   ```sh
   cp frontend/.env.example frontend/.env.local
   cd frontend
   pnpm install
   pnpm run dev
   ```

   Open the URL printed by Vite (usually `http://localhost:5173`). Set `VITE_API_URL` in `frontend/.env.local` if the API is not at `http://localhost:3000`.

Alternatively, build both applications and run the local preview/API together:

```sh
./scripts/build.sh
./scripts/start.sh
```

The combined preview uses port `4173` for the frontend and `3000` for the API. Set `UPSIDE_CONTACT` and `ALLOWED_ORIGIN=http://localhost:4173` in the shell before starting if needed.

## Environment

See [`backend/.env.example`](backend/.env.example) and [`frontend/.env.example`](frontend/.env.example) for the available settings. The backend README documents the API, cache, discovery providers, rate limits, deployment, and data-source policies in more detail.

## Data and attribution

The app queries OpenStreetMap services and public business websites. It displays OpenStreetMap attribution in the interface. Results may be incomplete or stale, and a missing website in OpenStreetMap is not proof that a business has no website. Respect provider usage policies, robots.txt, and request limits. See [`backend/README.md`](backend/README.md#discovery-and-data-use) for details.

The repository may include small offline JSON snapshots in `demo_data/` to support demo searches when live discovery is unavailable. These are OpenStreetMap-derived sample data, not a comprehensive or current business dataset; retain the source attribution when using them.

## Project layout

- `frontend/` — React, TypeScript, and Vite application
- `backend/` — Rust API and discovery/enrichment workers
- `demo_data/` — optional offline demo snapshots
- `scripts/` — build, local start, deploy, and demo cache warm-up scripts

## More documentation

- [Backend setup, API, data sources, and deployment](backend/README.md)
- [Frontend package scripts](frontend/package.json)
