# syntax=docker/dockerfile:1.7
FROM rust:1.96.0-bookworm AS rust-base
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config libssl-dev protobuf-compiler && rm -rf /var/lib/apt/lists/*
ARG CARGO_INDEX_URL=""
RUN if [ -n "$CARGO_INDEX_URL" ]; then \
    printf '[source.crates-io]\nreplace-with = "build-index"\n[source.build-index]\nregistry = "sparse+%s"\n' "$CARGO_INDEX_URL" > /usr/local/cargo/config.toml; \
    fi
WORKDIR /build

FROM rust-base AS decoder
RUN rustup target add wasm32-unknown-unknown
COPY wasm/alac-decoder/Cargo.toml wasm/alac-decoder/Cargo.lock ./wasm/alac-decoder/
# wasm-bindgen's CLI and library must use exactly the same version.
RUN version=$(sed -n '/name = "wasm-bindgen"/{n;s/version = "\(.*\)"/\1/p;}' wasm/alac-decoder/Cargo.lock) \
    && cargo install wasm-bindgen-cli --version "$version" --locked
COPY wasm/alac-decoder/src ./wasm/alac-decoder/src
RUN cargo build --manifest-path wasm/alac-decoder/Cargo.toml --locked \
    --target wasm32-unknown-unknown --release \
    && wasm-bindgen wasm/alac-decoder/target/wasm32-unknown-unknown/release/alac_decoder.wasm \
       --target web --typescript --out-dir /decoder
RUN apt-get update && apt-get install -y --no-install-recommends binaryen \
    && rm -rf /var/lib/apt/lists/* \
    && wasm-opt --enable-bulk-memory --enable-sign-ext --enable-nontrapping-float-to-int \
       -Oz /decoder/alac_decoder_bg.wasm -o /decoder/optimized.wasm \
    && mv /decoder/optimized.wasm /decoder/alac_decoder_bg.wasm

FROM node:24-bookworm-slim AS frontend
WORKDIR /build/frontend
COPY frontend/package.json frontend/package-lock.json ./
RUN npm ci --no-audit --no-fund
COPY frontend/ ./
COPY --from=decoder /decoder/ ./src/wasm/alac-decoder/
ARG VITE_MEDIA_BASE_URL=""
ARG VITE_LEGACY_MEDIA_BASE_URL=""
RUN npm run build

FROM rust-base AS backend
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release --locked --bin music-api

FROM python:3.13-slim-bookworm AS source
WORKDIR /source
COPY . .
RUN python3 scripts/package_source.py /source.tar.gz

FROM debian:bookworm-slim AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates ffmpeg curl libssl3 \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 10001 music \
    && useradd --uid 10001 --gid music --no-create-home --shell /usr/sbin/nologin music \
    && mkdir -p /app/data /app/config && chown music:music /app/data
WORKDIR /app
COPY --from=backend /build/target/release/music-api /usr/local/bin/music-api
COPY --from=frontend /build/web ./web
COPY --from=source /source.tar.gz ./web/source.tar.gz
COPY LICENSE NOTICE THIRD_PARTY_NOTICES.txt ./
ENV MUSIC_API_BIND=0.0.0.0:8000 \
    MUSIC_API_DATA_DIR=/app/data \
    MUSIC_API_CONFIG=/app/config/config.toml \
    MUSIC_API_SERVE_WEB=true \
    RUST_LOG=music_api=info,tower_http=info
USER 10001:10001
EXPOSE 8000
VOLUME ["/app/data"]
HEALTHCHECK --interval=30s --timeout=3s --start-period=20s --retries=3 \
    CMD curl --fail --silent --max-time 2 http://127.0.0.1:8000/health >/dev/null || exit 1
LABEL org.opencontainers.image.title="MHub" \
    org.opencontainers.image.description="Self-hosted music player with Rust, Vue and ALAC WebAssembly" \
    org.opencontainers.image.source="https://github.com/jizizr/mhub" \
    org.opencontainers.image.licenses="AGPL-3.0-only"
CMD ["music-api"]
