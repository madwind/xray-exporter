[dockerhub](https://hub.docker.com/r/madwind/xray-exporter)

```yaml
env:
  XRAY-API: 127.0.0.1:8080
  POR: 9100
```

| Metric | Description | Labels |
| :----- | :---------- | :----- |
| `xray_traffic_bytes_total` | Xray traffic statistics | `direction\|name\|type` |
| `xray_up` | Whether Xray is reachable | - |
| `xray_user_ip_online` | User online status per IP | `ip\|name` |

```prometheus

# HELP xray_traffic_bytes_total Xray traffic statistics
# TYPE xray_traffic_bytes_total counter
xray_traffic_bytes_total{direction="downlink",name="A",type="user"} 1.5255578e+07
xray_traffic_bytes_total{direction="downlink",name="B",type="user"} 4.901383506e+09
xray_traffic_bytes_total{direction="downlink",name="accept",type="inbound"} 4.916794688e+09
xray_traffic_bytes_total{direction="downlink",name="direct",type="outbound"} 4.916639078e+09
xray_traffic_bytes_total{direction="uplink",name="A",type="user"} 738059
xray_traffic_bytes_total{direction="uplink",name="B",type="user"} 1.11753509e+08
xray_traffic_bytes_total{direction="uplink",name="accept",type="inbound"} 1.15594141e+08
xray_traffic_bytes_total{direction="uplink",name="direct",type="outbound"} 1.04700802e+08
# HELP xray_up Whether Xray is reachable (1=up, 0=down)
# TYPE xray_up gauge
xray_up 1
# HELP xray_user_ip_online User online status per IP (1=online)
# TYPE xray_user_ip_online gauge
xray_user_ip_online{ip="1.2.3.4",name="B"} 1
````
