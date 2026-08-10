# syntax=docker/dockerfile:1.7
#
# MemVault runtime image.
#
# Two-stage build:
#   1. `builder`: full Rust toolchain, builds workspace in release mode
#   2. `runtime`: debian-slim with binaries + non-root user
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

# ===== Stage 1: builder ====================================================
FROM rust:1.83-slim-bookworm AS builder

RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      pkg-config libssl-dev ca-certificates tini \
 && rm -rf /var/lib/apt/lists/*

WORKDIR /build

# Cache layer: dependency manifest first so source edits skip registry rebuild.
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates

# Build with BuildKit cache mounts for registry + target.
# `--locked` 强制 Cargo.lock 锁版本,避免 CI/本地漂移。
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/build/target,sharing=locked \
    cargo build --release --workspace --locked \
 && cargo install --path crates/memvault-cli --locked --root /out \
 && cargo install --path crates/memvault-mcp --locked --root /out

# ===== Stage 2: runtime ====================================================
FROM debian:bookworm-slim AS runtime

RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      ca-certificates tini sqlite3 \
 && rm -rf /var/lib/apt/lists/* \
 && groupadd --system --gid 10001 memvault \
 && useradd  --system --uid 10001 --gid memvault --home /home/memvault --shell /sbin/nologin memvault \
 && mkdir -p /home/memvault/.memvault /etc/memvault \
 && chown -R memvault:memvault /home/memvault

COPY --from=builder /out/bin/memvault-cli  /usr/local/bin/
COPY --from=builder /out/bin/memvault-mcp  /usr/local/bin/

ENV MEMVAULT_DB=/home/memvault/.memvault/data.db \
    RUST_LOG=info \
    PATH=/home/memvault/.local/bin:$PATH

VOLUME ["/home/memvault/.memvault"]
WORKDIR /home/memvault

USER memvault

# tini 确保 MCP stdio 子进程能正确转发 SIGTERM
ENTRYPOINT ["/usr/bin/tini", "--"]
CMD ["memvault-mcp", "--db", "/home/memvault/.memvault/data.db"]

# Metadata
LABEL org.opencontainers.image.title="memvault" \
      org.opencontainers.image.description="AI Agent 时代的个人记忆路由器 (Memory Router)" \
      org.opencontainers.image.source="https://github.com/user/memvault" \
      org.opencontainers.image.licenses="MIT"