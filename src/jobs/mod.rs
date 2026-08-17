//! Background workers: queue expiry, match timers, and rating decay.
//!
//! Every sweep is idempotent and leased. The lease key includes the running
//! mode, so a debug process and a production process never contend, and two
//! processes in the same mode never run the same sweep twice.

use std::time::Duration as StdDuration;

use chrono::Duration;
use tokio::time::{interval, MissedTickBehavior};

use crate::observability::metrics;
use crate::services::match_svc::MatchService;
use crate::services::queue_svc::QueueService;
use crate::services::rating_svc::RatingService;
use crate::services::AppContext;

/// How often queue expiry and match timers are checked.
const TICK_SECONDS: u64 = 15;
/// How often rating decay runs.
const DECAY_SECONDS: u64 = 3_600;
/// How long a lease is held. Comfortably longer than a tick, so a slow sweep
/// keeps its lease, but short enough that a crashed process frees it quickly.
const LEASE_SECONDS: i64 = 60;

/// Identifies this process in the lock table, so a holder can renew its own
/// lease without waiting for it to expire.
fn holder_id() -> String {
    format!(
        "{}@{}",
        std::process::id(),
        hostname().unwrap_or_else(|| "unknown".to_string())
    )
}

fn hostname() -> Option<String> {
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Runs the timer loop until the process shuts down.
pub async fn run_timers(app: AppContext) {
    let holder = holder_id();
    let mut ticker = interval(StdDuration::from_secs(TICK_SECONDS));
    // A stalled tick must not cause a burst of catch-up sweeps.
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        ticker.tick().await;
        if !acquire(&app, "timers", &holder).await {
            continue;
        }
        tick_once(&app).await;
    }
}

/// One pass of the timer work: sweep expired queue slots, then advance every
/// match past a deadline.
///
/// Exposed so startup recovery can run it once before the loop begins, and so
/// tests can drive it directly.
pub async fn tick_once(app: &AppContext) {
    match QueueService::new(app.clone()).sweep_expired().await {
        Ok(0) => {}
        Ok(count) => {
            metrics::queue_expiries().add(count as u64);
            tracing::debug!(count, "removed expired queue members");
        }
        Err(error) => tracing::error!(%error, "queue expiry sweep failed"),
    }

    match MatchService::new(app.clone()).process_due().await {
        Ok(0) => {}
        Ok(count) => tracing::debug!(count, "advanced matches past a deadline"),
        Err(error) => tracing::error!(%error, "match timer sweep failed"),
    }
}

/// Runs rating decay on a slow loop.
pub async fn run_decay(app: AppContext) {
    let holder = holder_id();
    let mut ticker = interval(StdDuration::from_secs(DECAY_SECONDS));
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        ticker.tick().await;
        if !acquire(&app, "decay", &holder).await {
            continue;
        }
        decay_once(app.clone()).await;
    }
}

/// One pass of rating decay across every enabled channel.
///
/// Errors are logged per channel rather than aborting the sweep, so one
/// misconfigured channel cannot stop the rest.
pub async fn decay_once(app: AppContext) {
    let channels = match app.store.enabled_channels().await {
        Ok(channels) => channels,
        Err(error) => {
            tracing::error!(%error, "could not list channels for decay");
            return;
        }
    };
    let ratings = RatingService::new(app.clone());
    for channel in channels {
        match ratings.apply_decay(&channel).await {
            Ok(0) => {}
            Ok(count) => tracing::info!(channel = %channel.channel, count, "applied rating decay"),
            Err(error) => {
                tracing::error!(channel = %channel.channel, %error, "rating decay failed");
            }
        }
    }
}

/// Recovers in-flight state after a restart.
///
/// Nothing is rebuilt in memory: matches are stored with their deadlines, so
/// recovery is simply one immediate sweep. Anything that timed out while the
/// process was down is resolved on that pass.
pub async fn recover_on_startup(app: &AppContext) {
    let live = match app.store.all_live_matches().await {
        Ok(live) => live,
        Err(error) => {
            tracing::error!(%error, "could not load live matches during recovery");
            return;
        }
    };
    tracing::info!(count = live.len(), "resuming live matches");
    for loaded in &live {
        tracing::debug!(
            match_id = %loaded.info.id,
            state = loaded.info.state.as_str(),
            "resumed"
        );
    }
    tick_once(app).await;
}

/// Takes or renews the lease for a named job.
async fn acquire(app: &AppContext, name: &str, holder: &str) -> bool {
    let now = app.now();
    match app
        .store
        .try_acquire_job_lock(
            name,
            app.mode(),
            holder,
            now + Duration::seconds(LEASE_SECONDS),
            now,
        )
        .await
    {
        Ok(acquired) => acquired,
        Err(error) => {
            tracing::error!(job = name, %error, "could not take the job lease");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lease_outlives_a_tick() {
        assert!(
            LEASE_SECONDS > TICK_SECONDS as i64,
            "a sweep would lose its lease mid-run"
        );
    }

    #[test]
    fn the_holder_id_identifies_this_process() {
        let id = holder_id();
        assert!(id.contains(&std::process::id().to_string()));
        assert_eq!(id, holder_id(), "the id must be stable within a process");
    }
}
