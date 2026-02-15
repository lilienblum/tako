FROM rust:1.89-bookworm

RUN apt-get update \
  && apt-get install -y --no-install-recommends \
    openssh-client \
    curl \
    jq \
    cmake \
    pkg-config \
    ca-certificates \
    git \
  && rm -rf /var/lib/apt/lists/*

WORKDIR /workspace

COPY e2e/docker/runner/run-e2e.sh /usr/local/bin/run-e2e.sh
RUN chmod +x /usr/local/bin/run-e2e.sh

ENTRYPOINT ["/usr/local/bin/run-e2e.sh"]
