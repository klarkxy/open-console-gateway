# syntax=docker/dockerfile:1.7

FROM node:22.17.0-bookworm-slim AS web

ARG NPM_REGISTRY=https://registry.npmjs.org
WORKDIR /src
ENV npm_config_registry=$NPM_REGISTRY
RUN npm install --global pnpm@10.29.2
COPY package.json pnpm-lock.yaml pnpm-workspace.yaml ./
RUN pnpm install --frozen-lockfile --ignore-scripts --network-concurrency=1
COPY . .
RUN pnpm run build:web

FROM rust:1.88.0-bookworm AS cli

ARG CARGO_REGISTRY=sparse+https://index.crates.io/
WORKDIR /src
ENV CARGO_REGISTRIES_CRATES_IO_INDEX=$CARGO_REGISTRY \
    CARGO_HTTP_MULTIPLEXING=false \
    CARGO_HTTP_TIMEOUT=600 \
    CARGO_NET_RETRY=10
RUN if [ "$CARGO_REGISTRY" != "sparse+https://index.crates.io/" ]; then \
      printf '[source.crates-io]\nreplace-with = "mirror"\n\n[source.mirror]\nregistry = "%s"\n' \
        "$CARGO_REGISTRY" > /usr/local/cargo/config.toml; \
    fi
COPY . .
RUN --mount=type=cache,id=ocg-cargo-registry-cli,target=/usr/local/cargo/registry \
    --mount=type=cache,id=ocg-cargo-target-cli,target=/src/target \
    cargo build --locked --release -p ocg-manager-cli --no-default-features \
    && cp /src/target/release/ocg-manager-cli /ocg-manager-cli

FROM debian:bookworm-slim

RUN groupadd --gid 10001 ocg \
    && useradd --uid 10001 --gid 10001 --no-create-home \
      --home-dir /nonexistent --shell /usr/sbin/nologin ocg \
    && install -d -m 0700 -o ocg -g ocg \
      /data /browser-profiles /run/ocg-browser

COPY --from=cli /ocg-manager-cli /usr/local/bin/ocg-manager-cli
COPY --from=web /src/dist /opt/ocg-manager/dist
COPY LICENSE /usr/share/licenses/ocg-manager/LICENSE

USER ocg
VOLUME ["/data", "/browser-profiles", "/run/ocg-browser"]
EXPOSE 9042
STOPSIGNAL SIGINT
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD ["bash", "-c", "</dev/tcp/127.0.0.1/9042"]

ENTRYPOINT ["ocg-manager-cli"]
CMD ["--data-dir", "/data", "serve", "--host", "0.0.0.0", "--port", "9042", "--dashboard-dir", "/opt/ocg-manager/dist"]
