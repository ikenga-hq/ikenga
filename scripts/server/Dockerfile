# Runtime image for ikenga-server.
#
# This packages a PREBUILT binary + SPA rather than compiling in-image. Two
# reasons: the Rust crate dependency graph pulls in webkit2gtk/gtk system
# libraries even when only the `ikenga-server` bin is requested — compiling it
# here would mean carrying the full desktop toolchain in a server image.
#
# Build inputs come from `shell/scripts/server/deploy.sh`, which stages them into out/:
#
#   ./deploy.sh                                            # writes shell/scripts/server/out/{bin,dist}
#   docker build -f Dockerfile -t ikenga-server out        # from shell/scripts/server
#
# The build context is shell/scripts/server/out, NOT the repo root.

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    curl \
    git \
    tmux \
    procps \
    libssl3 \
    && rm -rf /var/lib/apt/lists/*

# Agents are spawned as child processes and several ship as JS; Bun is the
# runtime the sidecars expect.
RUN curl -fsSL https://bun.sh/install | bash
ENV PATH="/root/.bun/bin:${PATH}"

WORKDIR /opt/ikenga

COPY bin/ikenga-server /opt/ikenga/bin/ikenga-server
COPY dist /opt/ikenga/dist
RUN chmod +x /opt/ikenga/bin/ikenga-server

EXPOSE 4000
VOLUME ["/opt/ikenga/data", "/root/.claude", "/root/.gemini"]

# IKENGA_AUTH_TOKEN must be supplied at run time. The daemon fails closed and
# will mint + log a random token if it is unset, which rotates on every
# container restart — fine for a smoke test, wrong for anything persistent.
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
  CMD curl -fsS http://127.0.0.1:4000/api/health || exit 1

ENTRYPOINT ["/opt/ikenga/bin/ikenga-server", \
            "--host", "0.0.0.0", \
            "--port", "4000", \
            "--static-dir", "/opt/ikenga/dist", \
            "--data-dir", "/opt/ikenga/data"]
