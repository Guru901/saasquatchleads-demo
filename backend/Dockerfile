FROM rust:1.90-slim-bookworm AS builder

RUN apt-get update \
    && apt-get install -y --no-install-recommends pkg-config libssl-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build/backend

COPY backend/Cargo.toml backend/Cargo.lock ./
COPY backend/src ./src
COPY backend/scoring.json ./scoring.json

RUN cargo build --release --locked

FROM debian:bookworm-slim AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates libssl3 \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --create-home --uid 10001 app \
    && mkdir -p /app/demo_data \
    && chown -R app:app /app

WORKDIR /app

COPY --from=builder /build/backend/target/release/backend /usr/local/bin/backend
COPY --chown=app:app demo_data/ /app/demo_data/

ENV PORT=3000 \
    DEMO_DATA_DIR=/app/demo_data \
    RUST_LOG=info

USER app

EXPOSE 3000

CMD ["/usr/local/bin/backend"]
