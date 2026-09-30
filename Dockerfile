# syntax=docker/dockerfile:1

FROM rust:1.94-slim-trixie AS build
# pkg-config + libssl-dev: reqwest uses native-tls (system OpenSSL).
# protobuf-compiler: gtfs-realtime's build script runs protoc.
RUN apt-get update \
    && apt-get install -y --no-install-recommends pkg-config libssl-dev protobuf-compiler \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
# Public build: the auth tools drive a local Chrome, which a server doesn't have.
# Plain release, not release-optimized: fat LTO ran out of memory in a 2 GB
# builder and is 3-5x slower to build, for no gain on a network-bound server.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --locked --release --no-default-features \
    && cp target/release/slug-mcp /usr/local/bin/

FROM debian:trixie-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
# nutrition.sa.ucsc.edu (dining menus) sends the wrong intermediate for its
# certificate. Browsers and macOS fetch the right one via AIA; OpenSSL doesn't,
# so the image carries it. Drop this once
# `openssl s_client -connect nutrition.sa.ucsc.edu:443` verifies without it.
COPY certs/InCommon_ECC_OV_SSL_CA_3.crt /usr/local/share/ca-certificates/
RUN update-ca-certificates
# Config::load creates a data dir under $HOME, so the user needs a real home.
RUN useradd --system --create-home slug
USER slug
COPY --from=build /usr/local/bin/slug-mcp /usr/local/bin/slug-mcp
EXPOSE 3000
CMD ["slug-mcp", "serve", "--sse", "--host", "0.0.0.0", "--port", "3000"]
