//! Logging, metrics, and the health endpoint.
//!
//! Every log line, metric, and health response carries `mode`, so debug and
//! production output can never be confused for one another.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

use crate::config::{AppConfig, Mode};
use crate::repositories::Store;

/// Process-wide counters. Deliberately small: these are the numbers an
/// operator watches, not a general metrics framework.
pub mod metrics {
    use super::{AtomicU64, Counter};
    use std::sync::OnceLock;

    /// Declares a lazily initialised process-wide counter.
    macro_rules! counter {
        ($name:ident) => {
            #[doc = concat!("The `", stringify!($name), "` counter.")]
            pub fn $name() -> &'static Counter {
                static VALUE: OnceLock<Counter> = OnceLock::new();
                VALUE.get_or_init(|| Counter(AtomicU64::new(0)))
            }
        };
    }

    counter!(queue_joins);
    counter!(queue_expiries);
    counter!(matches_started);
    counter!(matches_completed);
    counter!(command_failures);

    /// Every counter as `(metric name, value)`, for the metrics endpoint.
    #[must_use]
    pub fn snapshot() -> Vec<(&'static str, u64)> {
        vec![
            ("pugbot_queue_joins_total", queue_joins().get()),
            ("pugbot_queue_expiries_total", queue_expiries().get()),
            ("pugbot_matches_started_total", matches_started().get()),
            ("pugbot_matches_completed_total", matches_completed().get()),
            ("pugbot_command_failures_total", command_failures().get()),
        ]
    }
}

/// A monotonically increasing counter, safe to share across tasks.
#[derive(Debug)]
pub struct Counter(AtomicU64);

impl Counter {
    /// Adds one.
    pub fn increment(&self) {
        self.add(1);
    }

    /// Adds `amount`.
    pub fn add(&self, amount: u64) {
        self.0.fetch_add(amount, Ordering::Relaxed);
    }

    /// The current value.
    #[must_use]
    pub fn get(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }
}

/// Installs the tracing subscriber.
///
/// Debug mode gets human-readable output with source locations; production gets
/// structured JSON for log aggregation. Both tag every event with the mode.
pub fn init_tracing(config: &AppConfig) {
    let filter = EnvFilter::try_new(&config.log_level)
        .unwrap_or_else(|_| EnvFilter::new("pugbot=info,warn"));

    let registry = tracing_subscriber::registry().with(filter);
    match config.mode {
        Mode::Debug => {
            registry
                .with(
                    tracing_subscriber::fmt::layer()
                        .with_file(true)
                        .with_line_number(true)
                        .with_target(true),
                )
                .init();
        }
        Mode::Production => {
            registry
                .with(tracing_subscriber::fmt::layer().json().with_target(true))
                .init();
        }
    }

    tracing::info!(mode = config.mode.as_str(), "logging initialised");
}

#[derive(Clone)]
struct HealthState {
    store: Arc<Store>,
    mode: &'static str,
    application_id: u64,
}

/// Serves `/health`, `/ready`, and `/metrics` until the process ends.
///
/// # Errors
///
/// Returns an error if the address cannot be bound or the server stops
/// unexpectedly.
pub async fn serve_health(
    bind: SocketAddr,
    store: Arc<Store>,
    config: &AppConfig,
) -> anyhow::Result<()> {
    let state = HealthState {
        store,
        mode: config.mode.as_str(),
        application_id: config.application_id,
    };

    let app = Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
        .route("/metrics", get(prometheus_metrics))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(bind).await?;
    tracing::info!(%bind, "health endpoint listening");
    axum::serve(listener, app).await?;
    Ok(())
}

/// Liveness: the process is running. Does not touch the database, so a database
/// outage does not cause a restart loop.
async fn health(State(state): State<HealthState>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "status": "ok",
        "mode": state.mode,
        "application_id": state.application_id,
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

/// Readiness: the database is reachable.
async fn ready(State(state): State<HealthState>) -> (StatusCode, Json<serde_json::Value>) {
    match state.store.ping().await {
        Ok(()) => (
            StatusCode::OK,
            Json(serde_json::json!({ "status": "ready", "mode": state.mode })),
        ),
        Err(error) => {
            tracing::warn!(%error, "readiness probe failed");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(serde_json::json!({
                    "status": "unavailable",
                    "mode": state.mode,
                    // The error text can carry connection details, so it is
                    // logged but never served.
                    "reason": "database unreachable",
                })),
            )
        }
    }
}

async fn prometheus_metrics(State(state): State<HealthState>) -> String {
    let mut output = String::new();
    for (name, value) in metrics::snapshot() {
        output.push_str(&format!("# TYPE {name} counter\n"));
        output.push_str(&format!("{name}{{mode=\"{}\"}} {value}\n", state.mode));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_start_at_zero_and_accumulate() {
        let counter = Counter(AtomicU64::new(0));
        assert_eq!(counter.get(), 0);
        counter.increment();
        counter.add(4);
        assert_eq!(counter.get(), 5);
    }

    #[test]
    fn the_metric_snapshot_names_everything_it_exports() {
        let snapshot = metrics::snapshot();
        assert!(!snapshot.is_empty());
        for (name, _) in snapshot {
            assert!(name.starts_with("pugbot_"), "{name} is not namespaced");
            assert!(name.ends_with("_total"), "{name} is a counter");
        }
    }

    #[test]
    fn global_counters_are_shared_across_calls() {
        let before = metrics::queue_joins().get();
        metrics::queue_joins().increment();
        assert_eq!(metrics::queue_joins().get(), before + 1);
    }
}
