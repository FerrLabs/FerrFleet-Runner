# syntax=docker/dockerfile:1
FROM rust:1.99-bookworm@sha256:e6f2cff35d0f309acf8433b4f05878ed77092602c5065cc81801177da0bb3a9c AS builder
WORKDIR /build

COPY Cargo.toml Cargo.lock ./
COPY shared ./shared
COPY runner ./runner

RUN cargo build --release --bin ferrfleet-runner

FROM debian:bookworm-slim@sha256:734a72b3d6142a71993b4bc580ca01d8ca687f77a2c422d96a961d3376b179bc AS runtime

# `image.source` is what the GHCR package page links to. Without it the package
# keeps whatever repository first published it, which here was the private
# FerrFleet-Cloud: a public image whose "source" link lands on a 404 for the
# people it was made public for.
LABEL org.opencontainers.image.source="https://github.com/FerrLabs/FerrFleet-Runner" \
      org.opencontainers.image.description="Executes one FerrFleet agent run." \
      org.opencontainers.image.licenses="Apache-2.0"

# `jq` and `python3` are here because agent prompts call them: without them a
# run still exits 0 while the step silently did nothing.
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      ca-certificates curl git jq python3 \
 && rm -rf /var/lib/apt/lists/*

# `gh` is deliberately absent. Agents reach for it on their own (no prompt asks
# for it), and shipping it would need a GitHub token in the run environment,
# which would route around the tool refusals the MCP GitHub server enforces
# (merge_pull_request, push_files, delete_ref, ...) via `gh pr merge` or
# `gh api`. The MCP server is the only GitHub path, and the system prompt says
# so. This holds on your runner exactly as it does on ours.

RUN useradd --create-home --uid 1000 --shell /bin/bash runner

ARG CLAUDE_CLI_VERSION=2.1.319
ENV PATH=/home/runner/.local/bin:$PATH \
    DISABLE_AUTOUPDATER=1
RUN su runner -c "curl -fsSL https://claude.ai/install.sh | bash -s -- '${CLAUDE_CLI_VERSION}'" \
 && /home/runner/.local/bin/claude --version

COPY --from=builder /build/target/release/ferrfleet-runner /usr/local/bin/ferrfleet-runner
COPY --from=grafana/mcp-grafana:2.3.0@sha256:c27248b5919d09d64ea273ca1b128b32902c6796f6e2dfcd11849d4f1363f9ed /app/mcp-grafana /usr/local/bin/mcp-grafana
RUN install -d -o runner -g runner /workdir \
 && git config --system --add safe.directory /workdir

USER runner
WORKDIR /workdir

ENTRYPOINT ["/usr/local/bin/ferrfleet-runner"]

FROM runtime AS formal

LABEL org.opencontainers.image.description="Executes one FerrFleet agent run, with a Rust toolchain, Kani and FerrFlow for formal verification."

USER root
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      build-essential pkg-config libssl-dev \
 && rm -rf /var/lib/apt/lists/*

USER runner
ENV RUSTUP_HOME=/home/runner/.rustup \
    CARGO_HOME=/home/runner/.cargo
ENV PATH=/home/runner/.cargo/bin:$PATH
ARG RUST_TOOLCHAIN=1.99.1
RUN curl -fsSL https://sh.rustup.rs | sh -s -- -y --no-modify-path \
      --profile minimal --default-toolchain "${RUST_TOOLCHAIN}" --component clippy,rustfmt \
 && cargo --version

ARG KANI_VERSION=0.68.0
RUN cargo install --locked kani-verifier --version "${KANI_VERSION}" \
 && cargo kani setup \
 && rm -rf "${CARGO_HOME}/registry" "${CARGO_HOME}/git"

COPY --chown=runner:runner --chmod=0755 formal/cbmc-limit.sh /home/runner/cbmc-limit.sh
RUN for cbmc in /home/runner/.kani/kani-*/bin/cbmc; do \
      mv "${cbmc}" "${cbmc}.real" && cp /home/runner/cbmc-limit.sh "${cbmc}"; \
    done \
 && rm /home/runner/cbmc-limit.sh

ARG SCCACHE_VERSION=0.19.0
ARG SCCACHE_SHA256=aa73c7270c997894038c0d8f0d297865f57569e1687883322bc431713d4bb9dd
RUN curl -fsSL -o /tmp/sccache.tar.gz \
      "https://github.com/mozilla/sccache/releases/download/v${SCCACHE_VERSION}/sccache-v${SCCACHE_VERSION}-x86_64-unknown-linux-musl.tar.gz" \
 && echo "${SCCACHE_SHA256}  /tmp/sccache.tar.gz" | sha256sum -c - \
 && tar -xzf /tmp/sccache.tar.gz -C /tmp \
 && install -m 0755 "/tmp/sccache-v${SCCACHE_VERSION}-x86_64-unknown-linux-musl/sccache" /home/runner/.cargo/bin/sccache \
 && rm -rf /tmp/sccache.tar.gz "/tmp/sccache-v${SCCACHE_VERSION}-x86_64-unknown-linux-musl" \
 && sccache --version

COPY --from=ghcr.io/ferrlabs/ferrflow:7.35.6@sha256:d67d0844a206a4f6104c4d082ad29253b57d2a56526ec777192535618a25a75b /usr/local/bin/ferrflow /usr/local/bin/ferrflow

ENV CARGO_HOME=/tmp/cargo \
    KANI_HOME=/home/runner/.kani

WORKDIR /workdir

FROM runtime AS default
