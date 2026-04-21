# ── Stage 1: Build ────────────────────────────────────────────────────────────
# Uses the official Rust image for a reproducible, hermetic build.
FROM rust:latest AS builder

WORKDIR /app

# Install compile-time system library headers required by egui/wayland/x11 crates.
RUN apt-get update && apt-get install -y --no-install-recommends \
        pkg-config \
        libwayland-dev \
        libxkbcommon-dev \
        libx11-dev \
        libxext-dev \
        libxi-dev \
        libxrandr-dev \
        libxcursor-dev \
        libxinerama-dev \
    && rm -rf /var/lib/apt/lists/*

# Copy manifests first so dependency compilation is cached in a separate layer.
# Rebuilding after source-only changes skips the slow dep compilation step.
COPY Cargo.toml Cargo.lock ./

# Build a stub binary to pre-compile and cache all dependencies.
RUN mkdir -p src && echo 'fn main() {}' > src/main.rs \
    && cargo build --release \
    && rm -f src/main.rs target/release/secure-note target/release/secure-note.d

# Copy the real source and build the final binary.
# touch updates timestamps so cargo sees source as newer than stub artifacts.
COPY src ./src
RUN find src -name "*.rs" -exec touch {} \; && cargo build --release

# ── Stage 2: Runtime image ────────────────────────────────────────────────────
# Minimal image for running the app inside Docker.
FROM debian:bookworm-slim AS runtime

WORKDIR /app

# Install runtime libraries required by the egui/wgpu Linux build.
RUN apt-get update && apt-get install -y --no-install-recommends \
        libx11-6 \
        libxext6 \
        libxi6 \
        libxrandr2 \
        libxcursor1 \
        libxinerama1 \
        libxkbcommon0 \
        libwayland-client0 \
        libvulkan1 \
        libgl1 \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/secure-note .

ENTRYPOINT ["/app/secure-note"]

# ── Stage 3: Export ───────────────────────────────────────────────────────────
# Scratch stage used with `docker build --output` to copy the binary to the
# host's ./build/ directory without needing docker cp or a running container.
FROM scratch AS export
COPY --from=builder /app/target/release/secure-note /secure-note
