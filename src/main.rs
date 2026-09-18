use std::{
    env,
    error::Error,
    fmt::Write as _,
    sync::{Arc, RwLock},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use axum::{Router, extract::State, http::header, response::IntoResponse, routing::get};
use prost::Message;
use tokio::{
    net::TcpListener,
    time::{Instant, Interval, MissedTickBehavior, interval, timeout},
};
use tonic::{
    Request, Status,
    client::Grpc,
    codegen::http::uri::PathAndQuery,
    transport::{Channel, Endpoint},
};
use tonic_prost::ProstCodec;

const SCRAPE_INTERVAL: Duration = Duration::from_secs(5);
const ONLINE_TTL: Duration = Duration::from_secs(15);
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
struct CollectorHealth {
    errors_total: u64,
    last_success_timestamp_seconds: f64,
}

impl CollectorHealth {
    fn record<T>(&mut self, collector: &str, result: Result<T, Status>) -> Option<T> {
        match result {
            Ok(value) => {
                self.last_success_timestamp_seconds = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs_f64();
                Some(value)
            }
            Err(error) => {
                self.errors_total = self.errors_total.saturating_add(1);
                eprintln!("Xray {collector} scrape failed: {error}");
                None
            }
        }
    }
}

#[derive(Clone, Debug, Default)]
struct MetricsSnapshot {
    up: bool,
    traffic: Vec<TrafficMetric>,
    online: Vec<OnlineMetric>,
    online_updated_at: Option<Instant>,
    scrape_duration_seconds: f64,
    traffic_health: CollectorHealth,
    online_health: CollectorHealth,
}

impl MetricsSnapshot {
    fn update(&mut self, result: ScrapeResult) {
        self.up = result.traffic.is_ok() && result.online.is_ok();
        self.scrape_duration_seconds = result.duration.as_secs_f64();
        if let Some(traffic) = self.traffic_health.record("traffic", result.traffic) {
            self.traffic = traffic;
        }
        if let Some(online) = self.online_health.record("online", result.online) {
            self.online = online;
            self.online_updated_at = Some(Instant::now());
        }
    }

    fn fresh_online(&self) -> &[OnlineMetric] {
        // Check at HTTP render time too, so expiry does not depend on the next scrape.
        if self
            .online_updated_at
            .is_some_and(|at| at.elapsed() < ONLINE_TTL)
        {
            &self.online
        } else {
            &[]
        }
    }
}

struct ScrapeResult {
    traffic: Result<Vec<TrafficMetric>, Status>,
    online: Result<Vec<OnlineMetric>, Status>,
    duration: Duration,
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

// Xray v26.9.9 app/stats/command/command.proto.
#[derive(Clone, PartialEq, Message)]
struct GetUsersStatsRequest {
    #[prost(bool, tag = "1")]
    include_traffic: bool,
    #[prost(bool, tag = "2")]
    reset: bool,
}

#[derive(Clone, PartialEq, Message)]
struct OnlineIPEntry {
    #[prost(string, tag = "1")]
    ip: String,
    #[prost(int64, tag = "2")]
    last_seen: i64,
}

#[derive(Clone, PartialEq, Message)]
struct UserStat {
    #[prost(string, tag = "1")]
    email: String,
    #[prost(message, repeated, tag = "2")]
    ips: Vec<OnlineIPEntry>,
    // Field 3 (traffic) is unused: QueryStats supplies the complete counters.
}

#[derive(Clone, PartialEq, Message)]
struct GetUsersStatsResponse {
    #[prost(message, repeated, tag = "1")]
    users: Vec<UserStat>,
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

    async fn get_users_stats(&self) -> Result<GetUsersStatsResponse, Status> {
        let mut grpc = self.inner.clone();
        grpc.ready()
            .await
            .map_err(|error| Status::unknown(format!("service not ready: {error}")))?;

        let request = Request::new(GetUsersStatsRequest {
            include_traffic: false,
            reset: false,
        });
        let path = PathAndQuery::from_static("/xray.app.stats.command.StatsService/GetUsersStats");
        let codec = ProstCodec::<GetUsersStatsRequest, GetUsersStatsResponse>::default();

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

fn scrape_schedule() -> Interval {
    let mut ticks = interval(SCRAPE_INTERVAL);
    ticks.set_missed_tick_behavior(MissedTickBehavior::Skip);
    ticks
}

async fn scrape_loop(state: SharedState, client: StatsClient) {
    let mut ticks = scrape_schedule();
    loop {
        ticks.tick().await;
        let result = scrape(&client).await;
        write_state(&state).update(result);
    }
}

async fn scrape(client: &StatsClient) -> ScrapeResult {
    let started = Instant::now();
    // Keep the two collectors independent. The outer timeouts also cover
    // channel readiness and response-body decoding, not just response headers.
    let (traffic, online) = tokio::join!(
        timeout(RPC_TIMEOUT, client.query_stats()),
        timeout(RPC_TIMEOUT, client.get_users_stats()),
    );
    let traffic = traffic
        .unwrap_or_else(|_| Err(Status::deadline_exceeded("QueryStats timed out")))
        .map(traffic_metrics);
    let online = online
        .unwrap_or_else(|_| Err(Status::deadline_exceeded("GetUsersStats timed out")))
        .map(online_metrics);

    ScrapeResult {
        traffic,
        online,
        duration: started.elapsed(),
    }
}

fn traffic_metrics(response: QueryStatsResponse) -> Vec<TrafficMetric> {
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

    traffic
}

fn online_metrics(response: GetUsersStatsResponse) -> Vec<OnlineMetric> {
    let mut online = Vec::new();
    for user in response.users {
        for entry in user.ips {
            online.push(OnlineMetric {
                name: user.email.clone(),
                ip: entry.ip,
            });
        }
    }
    online.sort_unstable_by(|a, b| (&a.name, &a.ip).cmp(&(&b.name, &b.ip)));
    online.dedup_by(|a, b| a.name == b.name && a.ip == b.ip);
    online
}

async fn metrics(State(state): State<SharedState>) -> impl IntoResponse {
    let snapshot = read_state(&state).clone();
    let body = render_metrics(&snapshot);

    (
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        body,
    )
}

fn render_metrics(snapshot: &MetricsSnapshot) -> String {
    let mut output =
        String::with_capacity(256 + snapshot.traffic.len() * 96 + snapshot.online.len() * 72);

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

    output.push_str("# HELP xray_up Whether both Xray collectors succeeded in the latest scrape (1=yes, 0=no)\n");
    output.push_str("# TYPE xray_up gauge\n");
    let _ = writeln!(output, "xray_up {}", u8::from(snapshot.up));

    output.push_str("# HELP xray_scrape_duration_seconds Duration of the latest Xray scrape\n");
    output.push_str("# TYPE xray_scrape_duration_seconds gauge\n");
    let _ = writeln!(
        output,
        "xray_scrape_duration_seconds {}",
        snapshot.scrape_duration_seconds
    );

    output.push_str("# HELP xray_scrape_errors_total Failed Xray scrapes by collector\n");
    output.push_str("# TYPE xray_scrape_errors_total counter\n");
    for (collector, health) in [
        ("traffic", &snapshot.traffic_health),
        ("online", &snapshot.online_health),
    ] {
        let _ = writeln!(
            output,
            "xray_scrape_errors_total{{collector=\"{collector}\"}} {}",
            health.errors_total
        );
    }

    output.push_str("# HELP xray_scrape_last_success_timestamp_seconds Unix time of last successful scrape by collector (0=never)\n");
    output.push_str("# TYPE xray_scrape_last_success_timestamp_seconds gauge\n");
    for (collector, health) in [
        ("traffic", &snapshot.traffic_health),
        ("online", &snapshot.online_health),
    ] {
        let _ = writeln!(
            output,
            "xray_scrape_last_success_timestamp_seconds{{collector=\"{collector}\"}} {}",
            health.last_success_timestamp_seconds
        );
    }

    output.push_str("# HELP xray_user_ip_online User online status per IP (1=online)\n");
    output.push_str("# TYPE xray_user_ip_online gauge\n");
    for metric in snapshot.fresh_online() {
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
    state
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn write_state(state: &SharedState) -> std::sync::RwLockWriteGuard<'_, MetricsSnapshot> {
    state
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

async fn shutdown_signal() {
    if let Err(error) = tokio::signal::ctrl_c().await {
        eprintln!("Failed to install shutdown signal handler: {error}");
    }
}
