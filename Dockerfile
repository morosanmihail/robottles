# ---- build stage ----
FROM rust:1-bookworm AS builder

WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --release

# ---- runtime stage ----
FROM debian:bookworm-slim

# git: robottles shells out to it to branch/commit/push in the target project.
# nodejs/npm: needed to install the `claude` CLI, the default agent runner.
RUN apt-get update && apt-get install -y --no-install-recommends \
      git ca-certificates curl gnupg \
    && curl -fsSL https://deb.nodesource.com/setup_20.x | bash - \
    && apt-get install -y --no-install-recommends nodejs \
    && npm install -g @anthropic-ai/claude-code \
    && apt-get purge -y curl gnupg \
    && apt-get autoremove -y \
    && rm -rf /var/lib/apt/lists/*

# Project checkouts are bind-mounted from the host, so they're almost never
# owned by the container's user; without this git refuses to touch them
# ("detected dubious ownership") even when running as root.
RUN git config --system --add safe.directory '*'

COPY --from=builder /build/target/release/robottles /usr/local/bin/robottles

WORKDIR /app
ENTRYPOINT ["robottles"]
CMD ["/app/config.yaml"]
