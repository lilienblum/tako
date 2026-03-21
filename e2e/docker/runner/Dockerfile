FROM debian:bookworm-slim

RUN apt-get update \
  && apt-get install -y --no-install-recommends \
    openssh-client \
    curl \
    unzip \
    jq \
    ca-certificates \
    git \
  && rm -rf /var/lib/apt/lists/*

ENV BUN_INSTALL=/root/.bun
RUN curl -fsSL https://bun.sh/install | bash \
  && ln -s "${BUN_INSTALL}/bin/bun" /usr/local/bin/bun

# Node.js + pnpm (for node fixtures)
RUN curl -fsSL https://deb.nodesource.com/setup_22.x | bash - \
  && apt-get install -y --no-install-recommends nodejs \
  && npm install -g pnpm \
  && rm -rf /var/lib/apt/lists/*

# Deno (for deno fixtures)
RUN curl -fsSL https://deno.land/install.sh | sh \
  && ln -s /root/.deno/bin/deno /usr/local/bin/deno

WORKDIR /workspace

COPY e2e/docker/runner/run-e2e.sh /usr/local/bin/run-e2e.sh
RUN chmod +x /usr/local/bin/run-e2e.sh

ENTRYPOINT ["/usr/local/bin/run-e2e.sh"]
