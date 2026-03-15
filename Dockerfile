# All-in-one SpaceNotes image
# Includes: SpacetimeDB + Module + Rust Daemon + MCP Server + Web Client

FROM clockworklabs/spacetime:v2.0.5 AS spacetime

# Chef stage - install cargo-chef
FROM rust:latest AS chef
RUN cargo install cargo-chef
WORKDIR /build

# Planner stage - generate recipe from dependencies
FROM chef AS planner
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY spacenotes-mcp ./spacenotes-mcp
COPY spacetime-module ./spacetime-module
RUN cargo chef prepare --recipe-path recipe.json

# Builder stage - cook dependencies first (cached), then build
FROM chef AS builder
RUN rustup target add wasm32-unknown-unknown

ARG TARGETARCH

COPY --from=planner /build/recipe.json recipe.json
RUN --mount=type=cache,target=/usr/local/cargo/registry,id=cargo-registry-${TARGETARCH} \
    --mount=type=cache,target=/build/target,id=cargo-target-${TARGETARCH} \
    cargo chef cook --release --recipe-path recipe.json

COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY spacenotes-mcp ./spacenotes-mcp
RUN --mount=type=cache,target=/usr/local/cargo/registry,id=cargo-registry-${TARGETARCH} \
    --mount=type=cache,target=/build/target,id=cargo-target-${TARGETARCH} \
    cargo build --release --package spacenotes --package spacenotes-mcp && \
    cp /build/target/release/spacenotes /build/spacenotes-bin && \
    cp /build/target/release/spacenotes-mcp /build/spacenotes-mcp-bin

COPY spacetime-module ./spacetime-module
WORKDIR /build/spacetime-module
RUN --mount=type=cache,target=/usr/local/cargo/registry,id=cargo-registry-${TARGETARCH} \
    --mount=type=cache,target=/build/spacetime-module/target,id=cargo-wasm-target-${TARGETARCH} \
    cargo build --release --target wasm32-unknown-unknown && \
    cp /build/spacetime-module/target/wasm32-unknown-unknown/release/spacenotes_module.wasm /build/spacenotes_module.wasm

# Runtime stage - Ubuntu 24.04 has glibc 2.39
FROM ubuntu:24.04

# Install dependencies
RUN apt-get update && apt-get install -y \
    ca-certificates \
    curl \
    nginx \
    && rm -rf /var/lib/apt/lists/*

# Copy SpacetimeDB from official image (both CLI and standalone server)
COPY --from=spacetime /opt/spacetime /opt/spacetime
RUN ln -s /opt/spacetime/spacetimedb-cli /usr/local/bin/spacetime && \
    ln -s /opt/spacetime/spacetimedb-standalone /usr/local/bin/spacetimedb-standalone

# Copy our daemon and MCP server
COPY --from=builder /build/spacenotes-bin /usr/local/bin/spacenotes
COPY --from=builder /build/spacenotes-mcp-bin /usr/local/bin/spacenotes-mcp

# Copy the pre-built WASM module
COPY --from=builder /build/spacenotes_module.wasm /opt/spacetime-module.wasm

# Copy the pre-built Flutter web client
COPY client-web /var/www/html

# Copy nginx config and entrypoint
COPY nginx-client.conf /etc/nginx/sites-available/default
COPY entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh

# Copy OpenCode config (will be exported to shared volume on startup)
COPY opencode.json /opt/opencode.json

# SpacetimeDB data directory
VOLUME /var/lib/spacetimedb

# Notes folder mount point
VOLUME /vault

# SpacetimeDB port, MCP port, Web client port
EXPOSE 3000 5052 80

ENV VAULT_PATH=/vault
ENV SPACETIME_HOST=http://127.0.0.1:3000
ENV SPACETIME_DB=spacenotes

ENTRYPOINT ["/entrypoint.sh"]
