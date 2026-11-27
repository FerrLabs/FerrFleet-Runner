# syntax=docker/dockerfile:1
FROM rust:1.100-bookworm@sha256:75d26c23c24cc6ea1cdabf0714a6bff8fd7bf3248716043fd7ae86dac7f41b94 AS builder
WORKDIR /build

COPY Cargo.toml Cargo.lock ./
COPY shared ./shared
COPY runner ./runner

RUN cargo build --release --bin ferrfleet-runner

FROM debian:bookworm-slim@sha256:e67eeead736ff7bbe258ef088bff683635c561b1fcfefad0806c1df4ab49ce0e AS runtime

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

ARG CLAUDE_CLI_VERSION=2.1.332
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

FROM runtime AS default
