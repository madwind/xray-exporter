# xray-exporter

Lightweight Prometheus exporter for Xray statistics.

## Configuration

| Variable | Default | Description |
| --- | --- | --- |
| `XRAY_API` | `127.0.0.1:8080` | Xray gRPC API address |
| `PORT` | `9100` | HTTP port for `/metrics` |

## Metrics

| Metric | Description | Labels |
| --- | --- | --- |
| `xray_traffic_bytes_total` | Xray traffic statistics | `type`, `name`, `direction` |
| `xray_up` | Whether Xray is reachable | - |
| `xray_user_ip_online` | User online status per IP | `name`, `ip` |

```prometheus
# HELP xray_traffic_bytes_total Xray traffic statistics
# TYPE xray_traffic_bytes_total counter
xray_traffic_bytes_total{type="user",name="A",direction="downlink"} 15255578

# HELP xray_up Whether Xray is reachable (1=up, 0=down)
# TYPE xray_up gauge
xray_up 1

# HELP xray_user_ip_online User online status per IP (1=online)
# TYPE xray_user_ip_online gauge
xray_user_ip_online{name="A",ip="1.2.3.4"} 1
```

## Docker

Image: `madwind/xray-exporter`
