# syntax=docker/dockerfile:1
FROM rust:1.95-slim-bookworm AS builder
RUN apt-get update \
    && apt-get install -y --no-install-recommends build-essential pkg-config libssl-dev cmake perl \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY . .
RUN cargo build --release --bin bilic

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates ffmpeg \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/bilic /usr/local/bin/bilic
ENV HOME=/data
WORKDIR /data
VOLUME ["/data"]
ENTRYPOINT ["bilic"]
