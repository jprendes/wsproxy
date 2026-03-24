# Build stage
FROM rust:1-alpine AS builder

RUN apk add --no-cache musl-dev

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src

RUN cargo build --release

# Runtime stage - scratch for minimal image
FROM scratch

COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/
COPY --from=builder /app/target/release/wsproxy /wsproxy

EXPOSE 8080

ENTRYPOINT ["/wsproxy"]
CMD ["server", "--listen", "0.0.0.0:8080"]
