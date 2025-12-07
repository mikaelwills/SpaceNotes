# All-in-one SpaceNotes image
# Includes: SpacetimeDB + Module + Rust Daemon

FROM clockworklabs/spacetime:v1.8.0 AS spacetime

# Build stage for Rust daemon AND SpacetimeDB module
FROM rust:latest AS builder

WORKDIR /build

# Install wasm target for SpacetimeDB module
RUN rustup target add wasm32-unknown-unknown

# Build the sync daemon
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release

# Build the SpacetimeDB module to WASM
COPY spacetime-module /build/spacetime-module
WORKDIR /build/spacetime-module
RUN cargo build --release --target wasm32-unknown-unknown

# Runtime stage
FROM debian:bookworm

# Install dependencies
RUN apt-get update && apt-get install -y \
    ca-certificates \
    curl \
    && rm -rf /var/lib/apt/lists/*

# Copy SpacetimeDB from official image (both CLI and standalone server)
COPY --from=spacetime /opt/spacetime /opt/spacetime
RUN ln -s /opt/spacetime/spacetimedb-cli /usr/local/bin/spacetime && \
    ln -s /opt/spacetime/spacetimedb-standalone /usr/local/bin/spacetimedb-standalone

# Copy our daemon
COPY --from=builder /build/target/release/spacenotes /usr/local/bin/spacenotes

# Copy the pre-built WASM module
COPY --from=builder /build/spacetime-module/target/wasm32-unknown-unknown/release/spacetime_module.wasm /opt/spacetime-module.wasm

# Copy entrypoint
COPY entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh

# SpacetimeDB data directory
VOLUME /var/lib/spacetimedb

# Notes folder mount point
VOLUME /vault

# SpacetimeDB port
EXPOSE 3000

ENV VAULT_PATH=/vault
ENV SPACETIME_HOST=http://127.0.0.1:3000
ENV SPACETIME_DB=spacenotes

ENTRYPOINT ["/entrypoint.sh"]
