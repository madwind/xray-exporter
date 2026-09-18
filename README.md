# xray-exporter

Lightweight Prometheus exporter for Xray statistics, targeting **Xray-core v26.9.9**.
Older Xray versions are not supported; there is no legacy API fallback.

## Configuration

| Variable | Default | Description |
| --- | --- | --- |
| `XRAY_API` | `127.0.0.1:8080` | Xray gRPC API address |
| `PORT` | `9100` | HTTP port for `/metrics` |

## Collection

Collection starts immediately and runs on a fixed **5-second** schedule. Missed
periods are skipped instead of replayed in a burst. The interval is not configurable.

Each round sends exactly two concurrent RPCs, regardless of user count:

- `QueryStats(pattern="", reset=false)` retrieves all traffic counters, including
  inbound/outbound counters and accumulated traffic for users who are now offline.
- `GetUsersStats(include_traffic=false, reset=false)` retrieves all online users
  and their IPs in one response.

Each RPC has a **3-second** deadline covering connection readiness and the complete
response. A failed collector does not prevent the other collector from updating.
The exporter does not reset Xray's counters.

Online IP snapshots expire **15 seconds** after their last successful update.
Expiry is checked when serving `/metrics`, so stale online samples disappear even
without another successful collection. A successful empty response removes all
previous online samples immediately. IP `last_seen` is not used for expiry:
a long-lived connection can remain online without a recent timestamp.

Traffic counters retain their last known values on failure. Check `xray_up` and
the per-collector last-success timestamps to detect stale data. Consider setting
Prometheus's `scrape_interval` to `5s` for matching visibility; shorter exporter
polling is not a substitute for connection-event logging.

## Metrics

| Metric | Description | Labels |
| --- | --- | --- |
| `xray_traffic_bytes_total` | Xray traffic statistics | `type`, `name`, `direction` |
| `xray_up` | 1 only if both collectors succeeded in the latest round; otherwise 0 | - |
| `xray_user_ip_online` | User online status per IP, subject to snapshot expiry | `name`, `ip` |
| `xray_scrape_duration_seconds` | Duration of the latest collection round | - |
| `xray_scrape_errors_total` | Failed RPC count since exporter startup | `collector` (`traffic` or `online`) |
| `xray_scrape_last_success_timestamp_seconds` | Unix time of the collector's last successful update, or 0 before its first success | `collector` (`traffic` or `online`) |

```prometheus
# HELP xray_traffic_bytes_total Xray traffic statistics
# TYPE xray_traffic_bytes_total counter
xray_traffic_bytes_total{type="user",name="A",direction="downlink"} 15255578

# HELP xray_up Whether both Xray collectors succeeded in the latest scrape (1=yes, 0=no)
# TYPE xray_up gauge
xray_up 1

# HELP xray_user_ip_online User online status per IP (1=online)
# TYPE xray_user_ip_online gauge
xray_user_ip_online{name="A",ip="1.2.3.4"} 1
```

Online IP metrics require Xray's `StatsService`, `stats: {}`, and
`policy.levels["<user level>"].statsUserOnline: true`. Each user must have a
non-empty `email` and use that policy level. The `name` label contains the user's
email. Only IPs reported as online by Xray are exported; when none are online or
the snapshot has expired, the metric has HELP/TYPE lines but no samples.

## Development

```sh
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

## Docker

Image: `madwind/xray-exporter`. The exporter does not include Xray-core; deploy
Xray-core v26.9.9 separately.