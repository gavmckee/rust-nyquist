# syntax=docker/dockerfile:1
FROM rust:1-slim-bookworm AS builder
WORKDIR /build

# libbpf-cargo compiles the BPF C programs with clang and links against
# libelf/zlib. (The nightly + bpf-linker install that used to live here
# served the retired Aya toolchain — the libbpf-rs path needs neither.)
RUN apt-get update && apt-get install -y --no-install-recommends \
    clang libelf-dev zlib1g-dev pkg-config make \
    && rm -rf /var/lib/apt/lists/*

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
