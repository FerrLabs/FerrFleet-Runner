# syntax=docker/dockerfile:1
FROM rust:1.99-bookworm@sha256:48b7f435b8fc6378ce43b44f15e3a47fd8919b2825f835c39bd4bd3da50af5b8 AS builder
WORKDIR /build

COPY Cargo.toml Cargo.lock ./
COPY shared ./shared
COPY runner ./runner

RUN cargo build --release --bin ferrfleet-runner

FROM debian:bookworm-slim@sha256:7ff6ba31527b9e336162fa061746c366c4b2032f4d2cff2078bea43f78efe3f8 AS runtime

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

ARG CLAUDE_CLI_VERSION=2.1.308
ENV PATH=/home/runner/.local/bin:$PATH
RUN su runner -c "curl -fsSL https://claude.ai/install.sh | bash -s -- '${CLAUDE_CLI_VERSION}'" \
 && /home/runner/.local/bin/claude --version

COPY --from=builder /build/target/release/ferrfleet-runner /usr/local/bin/ferrfleet-runner
RUN install -d -o runner -g runner /workdir

USER runner
WORKDIR /workdir

ENTRYPOINT ["/usr/local/bin/ferrfleet-runner"]
