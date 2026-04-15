FROM rust:1.91-bookworm AS builder
WORKDIR /app

COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY migrations ./migrations
COPY prompts ./prompts
COPY sql ./sql

RUN cargo build --release --bin ancilla-server

FROM debian:bookworm-slim
WORKDIR /app

RUN apt-get update \
  && apt-get install -y --no-install-recommends ca-certificates \
  && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/ancilla-server /usr/local/bin/ancilla-server
COPY --from=builder /app/migrations /app/migrations
COPY --from=builder /app/prompts /app/prompts
COPY --from=builder /app/sql /app/sql

EXPOSE 3000

ENTRYPOINT ["/usr/local/bin/ancilla-server"]
CMD ["serve", "--bind", "0.0.0.0:3000"]
