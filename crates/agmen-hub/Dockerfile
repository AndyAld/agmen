# syntax=docker/dockerfile:1
# The agmen hub image. Build from the repository root:
#   docker build -f crates/agmen-hub/Dockerfile -t agmen-hub:dev .
#
# The builder matches CI (ci/kubePod.yaml) and rust-toolchain.toml. The
# runtime is distroless cc on Debian 12, the same glibc the builder links
# against; it carries CA certificates and runs as uid 65532 (nonroot). The
# runtime has no tag that pins a build, so it is pinned by digest (checked
# 2026-10-25).

FROM rust:1.99.1-bookworm AS build
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --locked --release -p agmen-hub --bin agmen-hub \
    && cp target/release/agmen-hub /agmen-hub
RUN mkdir -p /out/var/lib/agmen/broker && chmod 0750 /out/var/lib/agmen/broker

FROM gcr.io/distroless/cc-debian12:nonroot@sha256:9dac0a79194e45a7da0158a9c6da57b217585af0786db3845d1f0ec1a0dd182f
COPY --from=build /agmen-hub /usr/local/bin/agmen-hub
COPY --from=build --chown=65532:65532 /out/var/lib/agmen /var/lib/agmen
ENV AGMEN_DATA_DIR=/var/lib/agmen
USER 65532:65532
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/agmen-hub"]
CMD ["serve"]
