use std::{
    collections::HashMap,
    env,
    error::Error,
    fmt::Write as _,
    sync::{Arc, RwLock},
    time::Duration,
};

use axum::{
    Router,
    extract::State,
    http::header,
    response::IntoResponse,
    routing::get,
};
use prost::Message;
use tokio::{net::TcpListener, time::sleep};
use tonic::{
    Request, Status,
    client::Grpc,
    codegen::http::uri::PathAndQuery,
    transport::{Channel, Endpoint},
};
use tonic_prost::ProstCodec;

const SCRAPE_INTERVAL: Duration = Duration::from_secs(5);
const FAIL_INTERVAL: Duration = Duration::from_secs(15);
const RPC_TIMEOUT: Duration = Duration::from_secs(3);

type SharedState = Arc<RwLock<MetricsSnapshot>>;

#[derive(Debug)]
struct Config {
    xray_api: String,
    port: u16,
}

impl Config {
    fn from_env() -> Self {
        let xray_api = env::var("XRAY_API").unwrap_or_else(|_| "127.0.0.1:8080".to_owned());
        let port = env::var("PORT")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(9100);

        Self { xray_api, port }
    }
}

#[derive(Clone, Debug)]
struct TrafficMetric {
    kind: String,
    name: String,
    direction: String,
    value: i64,
}

#[derive(Clone, Debug)]
struct OnlineMetric {
    name: String,
    ip: String,
}

#[derive(Clone, Debug, Default)]
struct MetricsSnapshot {
    up: bool,
    traffic: Vec<TrafficMetric>,
    online: Vec<OnlineMetric>,
}

#[derive(Clone, PartialEq, Message)]
struct GetStatsRequest {
    #[prost(string, tag = "1")]
    name: String,
    #[prost(bool, tag = "2")]
    reset: bool,
}

#[derive(Clone, PartialEq, Message)]
struct Stat {
    #[prost(string, tag = "1")]
    name: String,
    #[prost(int64, tag = "2")]
    value: i64,
}

#[derive(Clone, PartialEq, Message)]
struct QueryStatsRequest {
    #[prost(string, tag = "1")]
    pattern: String,
    #[prost(bool, tag = "2")]
    reset: bool,
}

#[derive(Clone, PartialEq, Message)]
struct QueryStatsResponse {
    #[prost(message, repeated, tag = "1")]
    stat: Vec<Stat>,
}

#[derive(Clone, PartialEq, Message)]
struct GetStatsOnlineIpListResponse {
    #[prost(string, tag = "1")]
    name: String,
    #[prost(map = "string, int64", tag = "2")]
    ips: HashMap<String, i64>,
}

#[derive(Clone, PartialEq, Message)]
struct GetAllOnlineUsersRequest {}

#[derive(Clone, PartialEq, Message)]
struct GetAllOnlineUsersResponse {
    #[prost(string, repeated, tag = "1")]
    users: Vec<String>,
}

#[derive(Clone)]
struct StatsClient {
    inner: Grpc<Channel>,
}

impl StatsClient {
    fn new(channel: Channel) -> Self {
        Self {
            inner: Grpc::new(channel),
        }
    }

    async fn query_stats(&self) -> Result<QueryStatsResponse, Status> {
        let mut grpc = self.inner.clone();
        grpc.ready()
            .await
            .map_err(|error| Status::unknown(format!("service not ready: {error}")))?;

        let request = Request::new(QueryStatsRequest {
            pattern: String::new(),
            reset: false,
        });
        let path = PathAndQuery::from_static("/xray.app.stats.command.StatsService/QueryStats");
        let codec = ProstCodec::<QueryStatsRequest, QueryStatsResponse>::default();

        grpc.unary(request, path, codec)
            .await
            .map(|response| response.into_inner())
    }

    async fn get_all_online_users(&self) -> Result<GetAllOnlineUsersResponse, Status> {
        let mut grpc = self.inner.clone();
        grpc.ready()
            .await
            .map_err(|error| Status::unknown(format!("service not ready: {error}")))?;

        let request = Request::new(GetAllOnlineUsersRequest {});
        let path = PathAndQuery::from_static(
            "/xray.app.stats.command.StatsService/GetAllOnlineUsers",
        );
        let codec = ProstCodec::<GetAllOnlineUsersRequest, GetAllOnlineUsersResponse>::default();

        grpc.unary(request, path, codec)
            .await
            .map(|response| response.into_inner())
    }

    async fn get_online_ips(&self, user: &str) -> Result<GetStatsOnlineIpListResponse, Status> {
        let mut grpc = self.inner.clone();
        grpc.ready()
            .await
            .map_err(|error| Status::unknown(format!("service not ready: {error}")))?;

        let request = Request::new(GetStatsRequest {
            name: format!("user>>>{user}>>>online"),
            reset: false,
        });
        let path = PathAndQuery::from_static(
            "/xray.app.stats.command.StatsService/GetStatsOnlineIpList",
        );
        let codec = ProstCodec::<GetStatsRequest, GetStatsOnlineIpListResponse>::default();

        grpc.unary(request, path, codec)
            .await
            .map(|response| response.into_inner())
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    let config = Config::from_env();
    eprintln!("Starting Xray exporter {}...", env!("CARGO_PKG_VERSION"));

    let endpoint = if config.xray_api.contains("://") {
        config.xray_api.clone()
    } else {
        format!("http://{}", config.xray_api)
    };

    let channel = Endpoint::from_shared(endpoint)?
        .connect_timeout(RPC_TIMEOUT)
        .timeout(RPC_TIMEOUT)
        .connect_lazy();
    let client = StatsClient::new(channel);
    let state = Arc::new(RwLock::new(MetricsSnapshot::default()));

    let scrape_state = Arc::clone(&state);
    let scrape_task = tokio::spawn(async move {
        scrape_loop(scrape_state, client).await;
    });

    let app = Router::new()
        .route("/metrics", get(metrics))
        .with_state(state);
    let address = format!("0.0.0.0:{}", config.port);
    let listener = TcpListener::bind(&address).await?;

    eprintln!("Exporter listening on {address}/metrics");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    scrape_task.abort();
    let _ = scrape_task.await;
    Ok(())
}

async fn scrape_loop(state: SharedState, client: StatsClient) {
    let mut fail_count = 0_u32;
    sleep(Duration::from_secs(2)).await;

    loop {
        match scrape(&client).await {
            Ok(mut snapshot) => {
                snapshot.up = true;
                fail_count = 0;
                *write_state(&state) = snapshot;
            }
            Err(error) => {
                fail_count = fail_count.saturating_add(1);
                write_state(&state).up = false;
                eprintln!("Xray scrape failed: {error}");
            }
        }

        let interval = if fail_count >= 3 {
            FAIL_INTERVAL
        } else {
            SCRAPE_INTERVAL
        };
        sleep(interval).await;
    }
}

async fn scrape(client: &StatsClient) -> Result<MetricsSnapshot, Status> {
    let response = client.query_stats().await?;
    let mut traffic = Vec::new();

    for stat in response.stat {
        if stat.value == 0 {
            continue;
        }

        let mut parts = stat.name.split(">>>");
        let Some(kind) = parts.next() else { continue };
        let Some(name) = parts.next() else { continue };
        if parts.next() != Some("traffic") {
            continue;
        }
        let Some(direction) = parts.next() else {
            continue;
        };

        traffic.push(TrafficMetric {
            kind: kind.to_owned(),
            name: name.to_owned(),
            direction: direction.to_owned(),
            value: stat.value,
        });
    }

    traffic.sort_unstable_by(|a, b| {
        (&a.kind, &a.name, &a.direction).cmp(&(&b.kind, &b.name, &b.direction))
    });

    let mut users = client.get_all_online_users().await?.users;
    users.sort_unstable();
    users.dedup();

    let mut online = Vec::new();
    for user in users {
        match client.get_online_ips(&user).await {
            Ok(response) => {
                for ip in response.ips.into_keys() {
                    online.push(OnlineMetric {
                        name: user.clone(),
                        ip,
                    });
                }
            }
            Err(error) => {
                eprintln!("GetStatsOnlineIpList failed for {user}: {error}");
            }
        }
    }

    online.sort_unstable_by(|a, b| (&a.name, &a.ip).cmp(&(&b.name, &b.ip)));

    Ok(MetricsSnapshot {
        up: true,
        traffic,
        online,
    })
}

async fn metrics(State(state): State<SharedState>) -> impl IntoResponse {
    let snapshot = read_state(&state).clone();
    let body = render_metrics(&snapshot);

    ([
        (
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        ),
    ], body)
}

fn render_metrics(snapshot: &MetricsSnapshot) -> String {
    let mut output = String::with_capacity(
        256 + snapshot.traffic.len() * 96 + snapshot.online.len() * 72,
    );

    output.push_str("# HELP xray_traffic_bytes_total Xray traffic statistics\n");
    output.push_str("# TYPE xray_traffic_bytes_total counter\n");
    for metric in &snapshot.traffic {
        output.push_str("xray_traffic_bytes_total{type=\"");
        push_label_value(&mut output, &metric.kind);
        output.push_str("\",name=\"");
        push_label_value(&mut output, &metric.name);
        output.push_str("\",direction=\"");
        push_label_value(&mut output, &metric.direction);
        let _ = writeln!(output, "\"}} {}", metric.value);
    }

    output.push_str("# HELP xray_up Whether Xray is reachable (1=up, 0=down)\n");
    output.push_str("# TYPE xray_up gauge\n");
    let _ = writeln!(output, "xray_up {}", u8::from(snapshot.up));

    output.push_str("# HELP xray_user_ip_online User online status per IP (1=online)\n");
    output.push_str("# TYPE xray_user_ip_online gauge\n");
    for metric in &snapshot.online {
        output.push_str("xray_user_ip_online{name=\"");
        push_label_value(&mut output, &metric.name);
        output.push_str("\",ip=\"");
        push_label_value(&mut output, &metric.ip);
        output.push_str("\"} 1\n");
    }

    output
}

fn push_label_value(output: &mut String, value: &str) {
    for ch in value.chars() {
        match ch {
            '\\' => output.push_str("\\\\"),
            '"' => output.push_str("\\\""),
            '\n' => output.push_str("\\n"),
            _ => output.push(ch),
        }
    }
}

fn read_state(state: &SharedState) -> std::sync::RwLockReadGuard<'_, MetricsSnapshot> {
    state.read().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn write_state(state: &SharedState) -> std::sync::RwLockWriteGuard<'_, MetricsSnapshot> {
    state.write().unwrap_or_else(|poisoned| poisoned.into_inner())
}

async fn shutdown_signal() {
    if let Err(error) = tokio::signal::ctrl_c().await {
        eprintln!("Failed to install shutdown signal handler: {error}");
    }
}
