FROM rust:1-slim AS build
WORKDIR /src
# The dependency graph changes far less often than the source, so build it
# against a stub first and let the layer cache keep it.
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src && echo 'fn main() {}' > src/main.rs && echo '' > src/lib.rs \
    && cargo build --release --locked && rm -rf src
COPY src ./src
# cargo skips a rebuild when only the mtime changed and the stub is newer.
RUN touch src/main.rs src/lib.rs && cargo build --release --locked

FROM debian:stable-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --home /data hookline \
    && mkdir -p /data && chown hookline:hookline /data
COPY --from=build /src/target/release/hookline /usr/local/bin/hookline
USER hookline
WORKDIR /data
VOLUME /data
ENV HOOKLINE_DATABASE=/data/hookline.db \
    HOOKLINE_LISTEN=0.0.0.0:8080
EXPOSE 8080
# The health route is unauthenticated so that this needs no credential.
HEALTHCHECK --interval=30s --timeout=3s --start-period=5s \
    CMD ["/usr/local/bin/hookline", "health"]
ENTRYPOINT ["/usr/local/bin/hookline"]
CMD ["serve"]
