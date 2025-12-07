# Runtime stage - binary is pre-built locally
FROM alpine:3.19

RUN apk add --no-cache ca-certificates

COPY target/x86_64-unknown-linux-musl/release/spacenotes /usr/local/bin/

ENTRYPOINT ["spacenotes"]
