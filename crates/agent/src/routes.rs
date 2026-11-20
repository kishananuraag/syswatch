//! HTTP surface: `/api/health`, `/api/current`, the six migrated history and
//! hardware endpoints (`/api/graph`, `/api/history`, `/api/power`,
//! `/api/analytics`, `/api/battery`, `/api/thermal`), and the dashboard's
//! static files.
//!
//! # Shape parity is the contract
//!
//! `/api/health` returns exactly the Python server's four keys, in the same
//! order (`ok`, `pid`, `uptime_s`, `port`) with the same JSON types, so every
//! existing consumer — the tray, the single-instance probe, the supervisor —
//! can point at either implementation.
//!
//! `/api/current` returns the Python server's blocks (`cpu`, `ram`, `net`,
//! `temps_c`, `processes`, `proc_count`, `boot_ts`, `load`, `battery`, `gpu`,
//! `disk_io`, `disk`, `pressure`, `unavailable`, `memory`) plus four the agent
//! can answer (`power`, `volumes`, `collector`, `agent`). Extra keys are
//! additive: nothing in the UI reads a key the Python server did not send.
//!
//! The six migrated endpoints each name their implementation module in its own
//! doc comment and disclose which source answered (`sources`), because the
//! agent reads the collector's JSONL tiers and its own 1 s ring where the
//! Python server reads its in-memory rings.
//!
//! # Endpoints that are NOT here
//!
//! `consumers`, `apps`, `apps/series`, `logs`, `alerts`, `agent-events`,
//! `permissions`, `power/history` still live in `dashboard/server.py`, which
//! stays the live server. This agent answers them with an honest 404 that names
//! what it does implement, rather than a bare "not found" that looks like a bug.

use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Instant;

use axum::Router;
use axum::extract::{Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, extract::Request};
use serde_json::{Value, json};
use tower_http::cors::CorsLayer;
use tower_http::services::{ServeDir, ServeFile};

use crate::histdata::History;

/// What the handlers can see. The snapshot slot is written only by the sampler
/// thread and read by handlers, so a request never blocks on a sample.
#[derive(Clone)]
pub struct AppState {
    /// Process start, for `uptime_s` (`Instant`, monotonic — not the clock).
    pub start: Instant,
    /// The port actually bound, which is what `/api/health` must report.
    pub port: u16,
    pub snapshot: Arc<RwLock<Option<Value>>>,
    pub static_dir: PathBuf,
    pub logs_dir: PathBuf,
    /// `dashboard/data` — where `battery-history.jsonl` lives (read-only here).
    pub data_dir: PathBuf,
    /// Ring + collector reader, shared by graph/history/analytics/power.
    pub history: Arc<History>,
    pub tick_ms: u64,
}

const MIGRATED: &[&str] = &[
    "/api/health",
    "/api/current",
    "/api/sensors",
    "/api/series",
    "/api/consumers",
    "/api/apps",
    "/api/graph",
    "/api/history",
    "/api/power",
    "/api/analytics",
    "/api/battery",
    "/api/thermal",
];

pub fn router(state: AppState) -> Router {
    let static_dir = state.static_dir.clone();
    let index = ServeFile::new(static_dir.join("index.html"));
    Router::new()
        .route("/api/health", get(health))
        .route("/api/current", get(current))
        .route("/api/sensors", get(sensors_handler))
        .route("/api/series", get(series_handler))
        .route("/api/consumers", get(consumers_handler))
        .route("/api/apps", get(apps_handler))
        .route("/api/graph", get(graph_handler))
        .route("/api/history", get(history_handler))
        .route("/api/power", get(power_handler))
        .route("/api/analytics", get(analytics_handler))
        .route("/api/battery", get(battery_handler))
        .route("/api/thermal", get(thermal_handler))
        // The dashboard's own routes: "/" and "/analytics" both serve the SPA.
        .route_service("/", index.clone())
        .route_service("/analytics", index)
        .nest_service("/static", ServeDir::new(static_dir))
        .fallback(not_found)
        // CORS: the tray and any local page may poll the agent; the Python
        // server's own CSRF guard lives on its POST routes, which are not
        // migrated here (no mutation surface in this binary).
        .layer(CorsLayer::permissive())
        .with_state(state)
}

fn uptime_s(state: &AppState) -> f64 {
    let s = state.start.elapsed().as_secs_f64();
    (s * 1000.0).round() / 1000.0
}

/// `{"ok": true, "pid": N, "uptime_s": N, "port": N}` — identical shape to
/// `Handler.api_health()` in dashboard/server.py.
async fn health(State(state): State<AppState>) -> Json<Value> {
    Json(json!({
        "ok": true,
        "pid": std::process::id(),
        "uptime_s": uptime_s(&state),
        "port": state.port,
    }))
}

/// The latest sampler snapshot. 503 until the first one exists — the Python
/// server answers the same way ("first sample in progress") and no client
/// should ever read a half-built snapshot as zeroes.
async fn current(State(state): State<AppState>) -> Response {
    let snap = state.snapshot.read().ok().and_then(|g| g.clone());
    match snap {
        Some(v) => (StatusCode::OK, Json(v)).into_response(),
        None => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "error": "first sample in progress",
                "retry_after_ms": state.tick_ms,
            })),
        )
            .into_response(),
    }
}

async fn sensors_handler(State(state): State<AppState>) -> Json<Value> {
    Json(crate::sensors::payload(&state))
}

#[derive(serde::Deserialize)]
struct SeriesQuery {
    metric: Option<String>,
    from: Option<f64>,
    to: Option<f64>,
    max_points: Option<usize>,
}

async fn series_handler(State(state): State<AppState>, Query(q): Query<SeriesQuery>) -> Json<Value> {
    let reader = crate::series::SeriesReader::new(state.logs_dir.clone());
    let req = crate::series::SeriesRequest {
        metric: q.metric.unwrap_or_default(),
        from: q.from.unwrap_or(0.0),
        to: q.to.unwrap_or(f64::MAX),
        max_points: q.max_points.unwrap_or(500),
    };
    Json(reader.series(&req))
}

#[derive(serde::Deserialize)]
struct ConsumersQuery {
    module: Option<String>,
    range: Option<String>,
    by: Option<String>,
    offset: Option<usize>,
    limit: Option<usize>,
}

async fn consumers_handler(State(state): State<AppState>, Query(q): Query<ConsumersQuery>) -> Json<Value> {
    let feed = crate::consumers::ConsumerFeed::new(state.logs_dir.clone());
    Json(feed.consumers(
        q.module.as_deref().unwrap_or("cpu"),
        q.range.as_deref().unwrap_or("1h"),
        q.offset.unwrap_or(0),
        q.limit.unwrap_or(50),
        q.by.as_deref().unwrap_or("process"),
    ))
}

async fn apps_handler(State(state): State<AppState>, Query(q): Query<ConsumersQuery>) -> Json<Value> {
    let feed = crate::consumers::ConsumerFeed::new(state.logs_dir.clone());
    Json(feed.apps(q.range.as_deref().unwrap_or("1h")))
}

#[derive(serde::Deserialize)]
struct GraphQuery {
    metric: Option<String>,
    range: Option<String>,
    from: Option<String>,
    to: Option<String>,
    max_points: Option<usize>,
}

/// `/api/graph` — see [`crate::graph`].
///
/// `from`/`to` accept unix seconds or an ISO-8601 timestamp (the Python server
/// accepts both); anything unparseable is ignored rather than guessed at.
async fn graph_handler(State(state): State<AppState>, Query(q): Query<GraphQuery>) -> Json<Value> {
    let req = crate::graph::GraphRequest {
        metric: q.metric.unwrap_or_default(),
        range: q.range,
        from: q.from.as_deref().and_then(parse_epoch),
        to: q.to.as_deref().and_then(parse_epoch),
        max_points: q.max_points.unwrap_or(crate::histdata::MAX_POINTS).clamp(1, 5_000),
    };
    Json(crate::graph::payload(
        &state.history,
        &req,
        crate::timefmt::now_unix() as f64,
    ))
}

#[derive(serde::Deserialize)]
struct HistoryQuery {
    range: Option<String>,
    metric: Option<String>,
}

/// `/api/history` — see [`crate::history`]. An unknown range is a 400 with the
/// list of valid ones, exactly like the Python server.
async fn history_handler(State(state): State<AppState>, Query(q): Query<HistoryQuery>) -> Response {
    let req = crate::history::HistoryRequest {
        range: q.range.unwrap_or_else(|| "1h".to_string()),
        metric: q.metric,
    };
    match crate::history::payload(&state.history, &req, crate::timefmt::now_unix() as f64) {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(msg) => (StatusCode::BAD_REQUEST, Json(json!({"error": msg}))).into_response(),
    }
}

/// `/api/power` — see [`crate::power`]. Always 200: a missing root daemon is a
/// normal state and the payload names it in `root_sample.reason`.
async fn power_handler(State(state): State<AppState>) -> Json<Value> {
    Json(crate::power::payload(&state.history))
}

#[derive(serde::Deserialize)]
struct AnalyticsQuery {
    range: Option<String>,
}

/// `/api/analytics` — see [`crate::analytics`].
async fn analytics_handler(
    State(state): State<AppState>,
    Query(q): Query<AnalyticsQuery>,
) -> Response {
    let req = crate::analytics::AnalyticsRequest {
        range: q.range.unwrap_or_else(|| "1h".to_string()),
    };
    match crate::analytics::payload(
        &state.history,
        &state.logs_dir,
        &req,
        crate::timefmt::now_unix() as f64,
    ) {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(msg) => (StatusCode::BAD_REQUEST, Json(json!({"error": msg}))).into_response(),
    }
}

/// `/api/battery` — see [`crate::battery`].
async fn battery_handler(State(state): State<AppState>) -> Json<Value> {
    Json(crate::battery::payload(&state.data_dir))
}

/// `/api/thermal` — see [`crate::thermal`].
async fn thermal_handler(State(_state): State<AppState>) -> Json<Value> {
    Json(crate::thermal::payload())
}

/// Unix seconds from `1700000000`, `1700000000.5` or `2026-11-19T05:30:00Z`.
fn parse_epoch(s: &str) -> Option<f64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Ok(v) = s.parse::<f64>()
        && v.is_finite()
    {
        return Some(if v > 1e12 { v / 1000.0 } else { v });
    }
    crate::timefmt::parse_rfc3339(s)
}

/// JSON 404 for everything this binary does not implement yet, listing what it
/// does. The static-file fallback lands here too, which is why the body names
/// the static prefix.
async fn not_found(req: Request) -> Response {
    let path = req.uri().path().to_string();
    let body = json!({
        "error": "not found",
        "detail": if path.starts_with("/api/") {
            "this endpoint is not migrated to the Rust agent yet — dashboard/server.py still serves it"
        } else {
            "no such route or static file"
        },
        "path": path,
        "agent": format!("syswatch-agent {}", env!("CARGO_PKG_VERSION")),
        "implemented_api": MIGRATED,
        "static_prefix": "/static/",
    });
    let mut res = (StatusCode::NOT_FOUND, Json(body)).into_response();
    res.headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    res
}

/// Body of a `Response` as bytes — used by the tests.
#[cfg(test)]
async fn body_bytes(res: Response) -> Vec<u8> {
    axum::body::to_bytes(res.into_body(), 1 << 22)
        .await
        .expect("body")
        .to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn state(with_snapshot: bool) -> AppState {
        let static_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../dashboard/static");
        let logs_dir = std::env::temp_dir().join(format!(
            "syswatch-routes-test-{}",
            std::process::id()
        ));
        let s = AppState {
            start: Instant::now(),
            port: 8199,
            snapshot: Arc::new(RwLock::new(None)),
            data_dir: std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../dashboard/data"),
            static_dir,
            history: Arc::new(History::new(logs_dir)),
            logs_dir: std::env::temp_dir().join(format!(
                "syswatch-routes-logs-{}",
                std::process::id()
            )),
            tick_ms: 1000,
        };
        if with_snapshot {
            *s.snapshot.write().unwrap() = Some(json!({"ts": "2026-11-20T00:00:00Z"}));
        }
        s
    }

    async fn get(app: Router, uri: &str) -> (StatusCode, Value) {
        let res = app
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = res.status();
        let bytes = body_bytes(res).await;
        let v = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, v)
    }

    #[tokio::test]
    async fn health_matches_the_python_shape() {
        let (status, v) = get(router(state(false)), "/api/health").await;
        assert_eq!(status, StatusCode::OK);
        let obj = v.as_object().expect("object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(keys, vec!["ok", "pid", "port", "uptime_s"]);
        assert_eq!(v["ok"], json!(true));
        assert_eq!(v["port"], json!(8199));
        assert!(v["pid"].as_u64().unwrap() > 0);
        let up = v["uptime_s"].as_f64().unwrap();
        assert!((0.0..60.0).contains(&up), "uptime_s {up}");
    }

    #[tokio::test]
    async fn current_is_503_before_the_first_sample() {
        let (status, v) = get(router(state(false)), "/api/current").await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(v["error"], json!("first sample in progress"));
    }

    #[tokio::test]
    async fn current_serves_the_snapshot_when_present() {
        let (status, v) = get(router(state(true)), "/api/current").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(v["ts"], json!("2026-11-20T00:00:00Z"));
    }

    #[tokio::test]
    async fn unmigrated_api_is_an_honest_404() {
        let (status, v) = get(router(state(true)), "/api/logs?limit=5").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(v["error"], json!("not found"));
        assert!(
            v["implemented_api"]
                .as_array()
                .unwrap()
                .contains(&json!("/api/graph"))
        );
        assert!(v["detail"].as_str().unwrap().contains("not migrated"));
    }

    #[tokio::test]
    async fn the_six_migrated_endpoints_answer_with_json() {
        // No collector log in the temp dir: every one of these must still answer
        // 200 with a well-formed payload that says what it could not measure.
        for (uri, key) in [
            ("/api/graph", "metric"),
            ("/api/history", "range"),
            ("/api/power", "tiers"),
            ("/api/analytics", "ranges"),
            ("/api/battery", "sources"),
            ("/api/thermal", "sensors"),
        ] {
            let (status, v) = get(router(state(true)), uri).await;
            assert_eq!(status, StatusCode::OK, "{uri}");
            assert!(v.get(key).is_some(), "{uri} has no {key}: {v}");
        }
    }

    #[tokio::test]
    async fn graph_defaults_to_cpu_and_names_its_window() {
        let (status, v) = get(router(state(true)), "/api/graph?range=1h").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(v["metric"], json!("cpu"));
        assert_eq!(v["range"], json!("1h"));
        let window = v["to"].as_i64().unwrap() - v["from"].as_i64().unwrap();
        assert_eq!(window, 3600);
        assert!(v["ticks"].as_array().is_some());
        // Empty history on a fresh machine is a 200 with a reason.
        assert_eq!(v["available"], json!(false));
        assert!(v["unavailable_reason"].as_str().unwrap().contains("no cpu samples"));
    }

    #[tokio::test]
    async fn graph_rejects_an_unknown_metric_visibly() {
        let (status, v) = get(router(state(true)), "/api/graph?metric=watts").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(v["available"], json!(false));
        assert!(v["error"].as_str().unwrap().contains("unknown metric"));
        assert!(v["implemented_metrics"].as_array().unwrap().contains(&json!("cpu")));
    }

    #[tokio::test]
    async fn history_rejects_an_unknown_range_with_400() {
        let (status, v) = get(router(state(true)), "/api/history?range=2h").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(v["error"].as_str().unwrap().contains("range must be one of"));
    }

    #[tokio::test]
    async fn analytics_rejects_an_unknown_range_with_400() {
        let (status, v) = get(router(state(true)), "/api/analytics?range=9d").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(v["error"].as_str().unwrap().contains("range must be one of"));
    }

    #[tokio::test]
    async fn thermal_and_power_agree_about_the_root_daemon() {
        // Both read the same file behind the same staleness rule, so either both
        // have it or both say why not.
        let (_, power) = get(router(state(true)), "/api/power").await;
        let (_, thermal) = get(router(state(true)), "/api/thermal").await;
        assert_eq!(thermal["available"], power["root_sample"]["available"]);
    }

    #[test]
    fn from_and_to_accept_epochs_and_iso() {
        assert_eq!(parse_epoch("1795113019"), Some(1_795_113_019.0));
        assert_eq!(parse_epoch("1795113019000"), Some(1_795_113_019.0));
        assert_eq!(parse_epoch("2026-11-19T05:30:00Z"), Some(1_795_066_200.0));
        assert_eq!(parse_epoch("junk"), None);
        assert_eq!(parse_epoch(""), None);
    }

    #[tokio::test]
    async fn index_is_served_from_the_dashboard_static_dir() {
        let app = router(state(false));
        let res = app
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let ctype = res
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        assert!(ctype.starts_with("text/html"), "ctype {ctype}");
    }

    #[tokio::test]
    async fn static_assets_are_served() {
        let (status, _v) = get(router(state(false)), "/static/app.css").await;
        assert_eq!(status, StatusCode::OK);
    }
}
