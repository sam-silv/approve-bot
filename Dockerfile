FROM rust:1.96-bookworm AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --locked --release

FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --uid 10001 approve-bot
COPY --from=builder /app/target/release/approve-bot /usr/local/bin/approve-bot
USER approve-bot
EXPOSE 3000
ENTRYPOINT ["/usr/local/bin/approve-bot"]
