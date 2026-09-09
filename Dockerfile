FROM rust:1.98.1-alpine AS builder

WORKDIR /app
RUN apk add --no-cache musl-dev

COPY Cargo.toml ./
COPY src ./src

RUN cargo build --release

FROM scratch

WORKDIR /app
COPY --from=builder /app/target/release/xray-exporter ./xray-exporter

ENTRYPOINT ["./xray-exporter"]
