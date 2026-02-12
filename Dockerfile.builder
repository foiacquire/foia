# FOIAcquire Builder - Compiles Rust binary only
# Used by CI to pre-build binaries that runtime images can copy

FROM rust:alpine AS builder

ARG FEATURES="browser,postgres,redis-backend,gis"

RUN apk add --no-cache musl-dev

WORKDIR /build

# 1. Copy manifests only — this layer is cached until Cargo.toml/lock change
COPY Cargo.toml Cargo.lock ./
COPY crates/foiacquire/Cargo.toml crates/foiacquire/Cargo.toml
COPY crates/foiacquire-analysis/Cargo.toml crates/foiacquire-analysis/Cargo.toml
COPY crates/foiacquire-annotate/Cargo.toml crates/foiacquire-annotate/Cargo.toml
COPY crates/foiacquire-cli/Cargo.toml crates/foiacquire-cli/Cargo.toml
COPY crates/foiacquire-import/Cargo.toml crates/foiacquire-import/Cargo.toml
COPY crates/foiacquire-scrape/Cargo.toml crates/foiacquire-scrape/Cargo.toml
COPY crates/foiacquire-server/Cargo.toml crates/foiacquire-server/Cargo.toml

# 2. Create empty stubs so cargo can resolve the workspace and compile deps
RUN mkdir -p crates/foiacquire/src && echo "" > crates/foiacquire/src/lib.rs \
    && mkdir -p crates/foiacquire-analysis/src && echo "" > crates/foiacquire-analysis/src/lib.rs \
    && mkdir -p crates/foiacquire-annotate/src && echo "" > crates/foiacquire-annotate/src/lib.rs \
    && mkdir -p crates/foiacquire-cli/src && echo "fn main() {}" > crates/foiacquire-cli/src/main.rs \
    && mkdir -p crates/foiacquire-import/src && echo "" > crates/foiacquire-import/src/lib.rs \
    && mkdir -p crates/foiacquire-scrape/src && echo "" > crates/foiacquire-scrape/src/lib.rs \
    && mkdir -p crates/foiacquire-server/src && echo "" > crates/foiacquire-server/src/lib.rs

# 3. Build dependencies only — cached until Cargo.toml/lock change
RUN if [ -n "$FEATURES" ]; then \
      cargo build --release --features "$FEATURES"; \
    else \
      cargo build --release; \
    fi

# 4. Remove stubs, copy real source, rebuild only workspace crates
RUN rm -rf crates/*/src
COPY crates ./crates

RUN if [ -n "$FEATURES" ]; then \
      cargo build --release --features "$FEATURES"; \
    else \
      cargo build --release; \
    fi \
    && strip target/release/foia

CMD ["true"]
