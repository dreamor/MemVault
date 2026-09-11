# syntax=docker/dockerfile:1.7
#
# MemVault runtime image.
#
# Three-stage build (cargo-chef):
#   1. `base`: Rust toolchain + cargo-chef
#   2. `planner`: distills workspace manifests into recipe.json
#   3. `builder`: deps layer (cacheable) + workspace build
#   4. `runtime`: debian-slim with binaries + non-root user
#
# Build:
#   docker build -t memvault:local .
#
# Run CLI:
#   docker run --rm -v memvault-data:/home/memvault/.memvault \
#     memvault:local memvault-cli --help
#
# MCP stdio (Claude Desktop):
#   docker run --rm -i -v memvault-data:/home/memvault/.memvault \
#     memvault:local memvault-mcp --db /home/memvault/.memvault/data.db

# ===== Stage 1: base toolchain (shared by planner + builder) ================
FROM rust:1.88-slim-trixie AS base

RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      pkg-config libssl-dev ca-certificates g++ \
 && rm -rf /var/lib/apt/lists/*

# cargo-chef decouples dependency compilation from source: the deps layer's
# only input is recipe.json (manifest digest), so it stays a cachable layer
# that buildx `cache-to type=gha,mode=max` can actually export/restore.
# (BuildKit cache-mount state never leaves the runner — that is why the old
# --mount=type=cache build was cold on every CI run.)
RUN cargo install cargo-chef --locked

# ===== Stage 2: planner — summarize workspace manifests =====================
FROM base AS planner
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo chef prepare --recipe-path recipe.json

# ===== Stage 3: builder ====================================================
FROM base AS builder
WORKDIR /build

# Dependency-only build; input is recipe.json, so source edits cannot
# invalidate it. Primed once, then restored from the GHA cache in minutes.
COPY --from=planner /build/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json

# Real sources on top — only workspace crates recompile from here.
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
# `--locked` 强制 Cargo.lock 锁版本,避免 CI/本地漂移。
RUN cargo build --release --workspace --locked \
 && cargo install --path crates/memvault-cli --locked --root /out \
 && cargo install --path crates/memvault-mcp --locked --root /out \
 && cargo install --path crates/memvault-proxy --locked --root /out

# ===== Stage 2: runtime ====================================================
FROM debian:trixie-slim AS runtime

RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      ca-certificates tini \
 && rm -rf /var/lib/apt/lists/* \
 && groupadd --system --gid 10001 memvault \
 && useradd  --system --uid 10001 --gid memvault --home /home/memvault --shell /sbin/nologin memvault \
 && mkdir -p /home/memvault/.memvault \
 && chown -R memvault:memvault /home/memvault

COPY --from=builder /out/bin/memvault-cli   /usr/local/bin/
COPY --from=builder /out/bin/memvault-mcp   /usr/local/bin/
COPY --from=builder /out/bin/memvault-proxy /usr/local/bin/

ENV MEMVAULT_DB=/home/memvault/.memvault/data.db \
    RUST_LOG=info

VOLUME ["/home/memvault/.memvault"]
WORKDIR /home/memvault

USER memvault

# tini 确保 MCP stdio 子进程能正确转发 SIGTERM
ENTRYPOINT ["/usr/bin/tini", "--"]
CMD ["memvault-mcp", "--db", "/home/memvault/.memvault/data.db"]

# Metadata
# io.modelcontextprotocol.server.name is the MCP Registry's OCI ownership-
# verification annotation; its value MUST equal server.json's "name", or
# `mcp-publisher publish` fails validation (docs/modelcontextprotocol-io/
# package-types.mdx -> Docker/OCI Images).
LABEL org.opencontainers.image.title="memvault" \
      org.opencontainers.image.description="AI Agent 时代的个人记忆路由器 (Memory Router)" \
      org.opencontainers.image.source="https://github.com/dreamor/memvault" \
      org.opencontainers.image.licenses="MIT" \
      io.modelcontextprotocol.server.name="io.github.dreamor/memvault"