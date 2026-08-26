# syntax=docker/dockerfile:1
# All building, testing and serving happens in containers — the host needs
# only Docker (rootless works). See compose.yaml for the entry points.

# ---------------------------------------------------------------------------
# Toolchain: Rust + wasm32 + wasm-pack + binaryen, plus node/npm so the
# package.json scripts run verbatim (single source of truth for commands).
FROM rust:1.98-slim AS toolchain
# Node 24 + npm copied from the official image (same version the serve and
# test-js stages use; rust-slim's apt has no npm package).
COPY --from=node:24-slim /usr/local/bin/node /usr/local/bin/node
COPY --from=node:24-slim /usr/local/lib/node_modules /usr/local/lib/node_modules
RUN ln -s /usr/local/lib/node_modules/npm/bin/npm-cli.js /usr/local/bin/npm \
    && apt-get update \
    && apt-get install -y --no-install-recommends binaryen \
    && rm -rf /var/lib/apt/lists/*
RUN rustup target add wasm32-unknown-unknown
# cargo install is slower than a release-tarball download but immune to
# release-asset renames; the layer is built once and cached.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    cargo install wasm-pack --version 0.15.0 --locked
WORKDIR /work

# ---------------------------------------------------------------------------
# Build the WASM bundle from a clean copy of the sources.
FROM toolchain AS wasm-build
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/work/target \
    npm run build:wasm

# ---------------------------------------------------------------------------
# Serve: static capture page + WASM, with the COOP/COEP isolation headers
# (ADR-0001) sent by web/server.mjs. Runtime is node only — no toolchain.
FROM node:24-slim AS serve
WORKDIR /app
COPY --from=wasm-build /work/web ./web
ENV PORT=8787
EXPOSE 8787
USER node
CMD ["node", "web/server.mjs"]
