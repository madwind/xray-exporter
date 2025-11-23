FROM golang:1.25.4-trixie AS builder

WORKDIR /app

COPY . .

ARG BUILDTIME
ARG TARGETOS
ARG TARGETARCH

RUN CGO_ENABLED=0 \
    go build -o proxify -ldflags="-s -w" .

FROM scratch

WORKDIR /app
COPY --from=builder /app/xray-exporter .

ENTRYPOINT ["./xray-exporter"]
