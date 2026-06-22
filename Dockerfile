# syntax=docker/dockerfile:1
FROM rust:1-slim-bookworm AS builder
WORKDIR /build

# Dependencies for bpf-linker: needs system LLVM 14 headers to avoid
# building LLVM from source (which takes 20+ minutes).
RUN apt-get update && apt-get install -y --no-install-recommends \
    clang llvm-14-dev libelf-dev pkg-config \
    && ln -sf /usr/bin/llvm-config-14 /usr/local/bin/llvm-config \
    && rm -rf /var/lib/apt/lists/*

# Install nightly with rust-src (needed for BPF build-std=core)
RUN rustup toolchain install nightly --component rust-src

# Install bpf-linker against system LLVM 14
RUN cargo install bpf-linker

COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/build/target \
    cargo build --release --bin nyquist && \
    cp /build/target/release/nyquist /nyquist

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
    libelf1 \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /nyquist /usr/local/bin/nyquist
EXPOSE 9100
ENTRYPOINT ["/usr/local/bin/nyquist"]
