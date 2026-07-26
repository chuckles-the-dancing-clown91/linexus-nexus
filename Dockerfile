# Linexus Nexus — control-plane API gateway (Loco/axum).
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
WORKDIR /app
# Loco resolves its config/ folder relative to the working directory.
COPY --from=build /app/config ./config
COPY --from=build /app/target/release/linexus_nexus-cli /usr/local/bin/linexus_nexus-cli
USER linexus
EXPOSE 5150
# DATABASE_URL, NEXUS_SYSTEM_TOKEN, LINEXUS_LOGGER_URL, LINEXUS_ORCH_URL and
# LINEXUS_SERVICE_TOKEN are provided by the environment (see deploy/).
CMD ["linexus_nexus-cli", "start", "-e", "development", "-b", "0.0.0.0", "-p", "5150"]
