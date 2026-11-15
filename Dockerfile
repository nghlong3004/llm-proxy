# syntax=docker/dockerfile:1
FROM rust:1.100.0-slim-bookworm AS builder
WORKDIR /build
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY src ./src
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    cargo build --release --locked

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates libgcc-s1 \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 10001 llm-proxy \
    && useradd --uid 10001 --gid llm-proxy --no-create-home \
        --home-dir /var/lib/llm-proxy --shell /usr/sbin/nologin llm-proxy \
    && install -d -o 10001 -g 10001 -m 0700 /var/lib/llm-proxy/state
COPY --from=builder /build/target/release/llm-proxy /usr/local/bin/llm-proxy
COPY LICENSE /usr/share/licenses/llm-proxy/LICENSE
ENV LLM_PROXY_STATE_DIR=/var/lib/llm-proxy/state
USER 10001:10001
EXPOSE 18765
STOPSIGNAL SIGINT
ENTRYPOINT ["/usr/local/bin/llm-proxy"]
CMD ["serve", "--listen-address", "0.0.0.0", "--listen-port", "18765", "--default-model", "gpt-6.1-sol"]
