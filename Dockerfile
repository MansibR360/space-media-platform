# ---- build stage ----
FROM rust:1-bookworm AS builder
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo "fn main() {}" > src/main.rs && cargo build --release && rm -rf src target
COPY migrations ./migrations
COPY src ./src
ARG FEATURES=""
RUN touch src/main.rs && cargo build --release

# ---- runtime stage ----
FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates ffmpeg curl \
    && rm -rf /var/lib/apt/lists/*
RUN useradd -r -u 10001 space
WORKDIR /app
COPY --from=builder /build/target/release/space /usr/local/bin/space
COPY web ./web
USER space
ENV STORAGE_DIR=/data/storage
VOLUME ["/data/storage"]
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=3s CMD curl -fsS http://localhost:8080/health || exit 1
ENTRYPOINT ["space"]
