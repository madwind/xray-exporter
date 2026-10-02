FROM scratch

WORKDIR /app
COPY dist/xray-exporter ./xray-exporter

ENTRYPOINT ["./xray-exporter"]
