# The fno sandbox image. fno-agent embeds this recipe and builds it with a
# context that holds `bin/` (Linux fno binaries for the host rev, checked by
# sha256 from bin-cache, or a local directory such as CI's PR-head build) and
# `plugin/` (the staged plugin whose source head matches that rev, or empty).
# The worker runs as uid 1000 with no network; the host relay is its only
# way out.
FROM debian:bookworm-slim

ARG HARNESSES="@anthropic-ai/claude-code @openai/codex"

RUN apt-get update \
 && apt-get install -y --no-install-recommends git ca-certificates nodejs npm \
 && if [ -n "$HARNESSES" ]; then npm install -g $HARNESSES; fi \
 && npm cache clean --force \
 && rm -rf /var/lib/apt/lists/*

COPY bin/ /usr/local/bin/
COPY plugin/ /opt/fno/plugin/

RUN useradd --uid 1000 --create-home --home-dir /home/fno --shell /bin/sh fno \
 && mkdir -p /work /tmp/fno \
 && chown fno:fno /work /tmp/fno \
 && chmod 0700 /tmp/fno \
 && fno-agent version --json

ENV HOME=/home/fno \
    FNO_SUPERVISOR_SOCKET=/tmp/fno/supervisor.sock \
    CLAUDE_PLUGIN_ROOT=/opt/fno/plugin
USER 1000:1000
WORKDIR /work
