# =============================================================================
# ClawDesk — Multi-stage Docker Build
# =============================================================================
#
# Stage 1: cargo-chef (dependency caching)
# Stage 2: build (compile the full workspace)
# Stage 3: runtime (distroless image with just the binary)
#
# Usage:
#   docker build -t clawdesk .
#   docker run -p 18789:18789 clawdesk
#
# With environment:
#   docker run -p 18789:18789 \
#     -e ANTHROPIC_API_KEY=sk-... \
#     -e OPENAI_API_KEY=sk-... \
#     -v clawdesk-data:/data \
#     clawdesk
# =============================================================================

# ── Stage 1: Chef — compute dependency recipe ────────────────
FROM rust:1.82-bookworm AS chef

RUN cargo install cargo-chef --locked
WORKDIR /build

# ── Stage 2: Planner — generate recipe.json ──────────────────
FROM chef AS planner

COPY . .
RUN cargo chef prepare --recipe-path recipe.json

# ── Stage 3: Builder — cached dependency build + app build ───
FROM chef AS builder

# Install system dependencies
RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    protobuf-compiler \
    && rm -rf /var/lib/apt/lists/*

# Cook dependencies (cached layer — only rebuilds when deps change)
COPY --from=planner /build/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json

# Copy full source and build
COPY . .
RUN cargo build --release --bin clawdesk \
    && strip target/release/clawdesk

# ── Stage 4: Runtime — minimal image ─────────────────────────
FROM debian:bookworm-slim AS runtime

# Install runtime dependencies (TLS, CA certs)
RUN apt-get update && apt-get install -y \
    ca-certificates \
    libssl3 \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --shell /bin/bash clawdesk

# Copy the built binary
COPY --from=builder /build/target/release/clawdesk /usr/local/bin/clawdesk

# Data volume for SochDB persistence
VOLUME /data
ENV CLAWDESK_DATA_DIR=/data

# Switch to non-root user
USER clawdesk
WORKDIR /home/clawdesk

EXPOSE 18789

# Health check
HEALTHCHECK --interval=30s --timeout=3s --start-period=10s --retries=3 \
    CMD curl -sf http://localhost:18789/api/v1/health || exit 1

ENTRYPOINT ["clawdesk"]
CMD ["gateway", "run", "--port", "18789", "--bind", "all"]
