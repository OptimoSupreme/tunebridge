# Build stage. Always built on a workstation (see deploy.sh), never on the server.
FROM docker.io/library/rust:1-bookworm AS build
WORKDIR /src

# Cache dependency compilation separately from the app source.
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src/providers \
    && echo 'fn main() {}' > src/main.rs \
    && cargo build --release --locked \
    && rm -rf src target/release/tunebridge target/release/deps/tunebridge-*

COPY src ./src
RUN cargo build --release --locked

# Runtime stage.
FROM docker.io/library/debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates tzdata \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --create-home --home-dir /data tunebridge
COPY --from=build /src/target/release/tunebridge /usr/local/bin/tunebridge
USER tunebridge
WORKDIR /data
VOLUME ["/data"]
ENV TUNEBRIDGE_CONFIG=/config.toml \
    RUST_LOG=tunebridge=info,matrix_sdk=warn
ENTRYPOINT ["/usr/local/bin/tunebridge"]
