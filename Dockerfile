# All-in-one SpaceNotes image
# Includes: SpacetimeDB + Module + Rust Daemon

FROM clockworklabs/spacetime:v1.8.0 AS spacetime

# Build stage for Rust daemon
FROM rust:latest AS builder

WORKDIR /build

# Copy and build (native architecture)
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release

# Runtime stage
FROM debian:bookworm-slim

# Install dependencies
RUN apt-get update && apt-get install -y \
    ca-certificates \
    curl \
    && rm -rf /var/lib/apt/lists/*

# Copy SpacetimeDB from official image
COPY --from=spacetime /usr/local/bin/spacetime /usr/local/bin/spacetime

# Copy our daemon
COPY --from=builder /build/target/release/spacenotes /usr/local/bin/spacenotes

# Copy the SpacetimeDB module source
COPY spacetime-module /opt/spacetime-module

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
