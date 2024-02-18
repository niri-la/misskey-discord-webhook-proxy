FROM rust:1 as builder

COPY . /app
WORKDIR /app

RUN cargo build --release

FROM gcr.io/distroless/cc-debian12

COPY --from=builder /app/target/release/misskey-discord-webhook-proxy /

ENTRYPOINT ["/misskey-discord-webhook-proxy"]
CMD ["0.0.0.0:3000"]
