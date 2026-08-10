# Linexus Nexus — API gateway / control plane (Loco).
FROM rust:1-slim AS build
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config build-essential ca-certificates git && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY . .
RUN cargo build --release --bin linexus_nexus-cli

FROM debian:stable-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 linexus \
    && mkdir -p /var/lib/linexus && chown linexus /var/lib/linexus
COPY --from=build /app/target/release/linexus_nexus-cli /usr/local/bin/linexus_nexus-cli
# Loco resolves ./config relative to the working directory.
COPY config /app/config
WORKDIR /app
USER linexus
ENV DATABASE_URL=sqlite:///var/lib/linexus/nexus.sqlite?mode=rwc
EXPOSE 5150
# config/development.yaml binds localhost, so -b 0.0.0.0 is required to be
# reachable from other containers. production.yaml is a gitignored empty
# placeholder — development is the only runnable environment today.
CMD ["linexus_nexus-cli", "start", "-e", "development", "-b", "0.0.0.0", "-p", "5150"]
