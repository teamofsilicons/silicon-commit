# syntax=docker/dockerfile:1.7

FROM rust:1.98.0-bookworm AS builder
WORKDIR /workspace
ARG GIT_COMMIT_SHA=unknown
ENV GIT_COMMIT_SHA=${GIT_COMMIT_SHA}

COPY Cargo.toml Cargo.lock ./
COPY migrations ./migrations
COPY src ./src

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/workspace/target \
    cargo build --locked --release --bins && \
    cp target/release/commit-api /tmp/commit-api && \
    cp target/release/commit-worker /tmp/commit-worker && \
    cp target/release/commit-migrate /tmp/commit-migrate

FROM debian:bookworm-slim AS runtime
RUN apt-get update && \
    apt-get install --yes --no-install-recommends ca-certificates && \
    rm -rf /var/lib/apt/lists/* && \
    groupadd --gid 10001 commit && \
    useradd --uid 10001 --gid commit --no-create-home --shell /usr/sbin/nologin commit

COPY --from=builder /tmp/commit-api /usr/local/bin/commit-api
COPY --from=builder /tmp/commit-worker /usr/local/bin/commit-worker
COPY --from=builder /tmp/commit-migrate /usr/local/bin/commit-migrate

USER 10001:10001
EXPOSE 8080
CMD ["commit-api"]
