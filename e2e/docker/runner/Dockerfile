FROM rust:1.89-bookworm

RUN apt-get update \
  && apt-get install -y --no-install-recommends \
    openssh-client \
    curl \
    unzip \
    jq \
    cmake \
    pkg-config \
    ca-certificates \
    git \
    docker.io \
  && rm -rf /var/lib/apt/lists/*

ENV BUN_INSTALL=/root/.bun
RUN curl -fsSL https://bun.sh/install | bash \
  && ln -s "${BUN_INSTALL}/bin/bun" /usr/local/bin/bun

WORKDIR /workspace

COPY e2e/docker/runner/run-e2e.sh /usr/local/bin/run-e2e.sh
RUN chmod +x /usr/local/bin/run-e2e.sh

ENTRYPOINT ["/usr/local/bin/run-e2e.sh"]
