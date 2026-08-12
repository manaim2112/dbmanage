FROM rust:1.85-slim AS builder
WORKDIR /app
COPY . .
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
    mariadb-client postgresql-client ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/dbmanage /usr/local/bin/dbmanage
COPY --from=builder /app/static /app/static
WORKDIR /app
ENV DBMANAGE_DATA_DIR=/data
EXPOSE 3000
CMD ["dbmanage"]
