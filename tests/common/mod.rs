//! Shared setup for the database integration tests.
//!
//! These tests need a real PostgreSQL instance because most of what they check
//! is enforced by the schema: unique constraints, partial indexes, and
//! transactional behaviour. Set `PUGBOT_TEST_DATABASE_URL` to run them; without
//! it each test skips with a message rather than failing, so `cargo test` works
//! on a machine with no database.
//!
//! Each test gets its own PostgreSQL schema, created and migrated from
//! scratch, so the suite runs in parallel without tests clobbering one
//! another — and every test also exercises the migrations on an empty schema.
//! Point this at a throwaway database: schemas are dropped and recreated.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use pugbot::config::{AppConfig, Mode};
use pugbot::domain::clock::{Clock, FakeClock};
use pugbot::domain::ids::{ChannelId, GuildId};
use pugbot::domain::settings::{ChannelSettings, QueueSettings};
use pugbot::repositories::Store;
use pugbot::services::{AppContext, NullNotifier};
use sqlx::postgres::PgPoolOptions;

/// Skips the calling test when no test database is configured.
#[macro_export]
macro_rules! require_database {
    () => {
        match $crate::common::test_database_url() {
            Some(url) => url,
            None => {
                eprintln!(
                    "skipping: set PUGBOT_TEST_DATABASE_URL to run the database integration tests"
                );
                return;
            }
        }
    };
}

pub fn test_database_url() -> Option<String> {
    std::env::var("PUGBOT_TEST_DATABASE_URL")
        .ok()
        .filter(|url| !url.trim().is_empty())
}

/// Everything a test needs: a migrated, empty database and a context wired to
/// a clock the test controls.
pub struct TestApp {
    pub app: AppContext,
    pub store: Arc<Store>,
    pub clock: FakeClock,
    pub guild: GuildId,
    pub channel: ChannelId,
}

/// Distinguishes concurrently running tests within one process.
static SCHEMA_COUNTER: AtomicU32 = AtomicU32::new(0);

impl TestApp {
    pub async fn start(url: &str) -> Self {
        let schema = format!(
            "pugbot_test_{}_{}",
            std::process::id(),
            SCHEMA_COUNTER.fetch_add(1, Ordering::Relaxed)
        );

        // A short-lived connection creates the schema; the real pool then
        // pins every connection to it.
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect(url)
            .await
            .expect("connect to the test database");
        for statement in [
            format!("DROP SCHEMA IF EXISTS {schema} CASCADE"),
            format!("CREATE SCHEMA {schema}"),
        ] {
            sqlx::query(&statement)
                .execute(&admin)
                .await
                .unwrap_or_else(|error| panic!("{statement}: {error}"));
        }
        admin.close().await;

        let pinned = schema.clone();
        let pool = PgPoolOptions::new()
            .max_connections(5)
            .after_connect(move |conn, _meta| {
                let pinned = pinned.clone();
                Box::pin(async move {
                    sqlx::Executor::execute(
                        &mut *conn,
                        format!("SET search_path TO {pinned}").as_str(),
                    )
                    .await?;
                    Ok(())
                })
            })
            .connect(url)
            .await
            .expect("connect to the test schema");

        let store = Arc::new(Store::from_pool(pool));
        store.migrate().await.expect("apply migrations");

        let clock = FakeClock::at_epoch();
        let config = Arc::new(test_config(url));
        let app = AppContext::new(
            Arc::clone(&store),
            config,
            Arc::new(clock.clone()),
            Arc::new(NullNotifier),
        );

        Self {
            app,
            store,
            clock,
            guild: GuildId(900_000_000_000_000_001),
            channel: ChannelId(900_000_000_000_000_002),
        }
    }

    /// Enables the channel and creates its queue.
    pub async fn with_queue(&self, settings: QueueSettings) -> pugbot::repositories::QueueRow {
        self.with_queue_and_channel(settings, ChannelSettings::default())
            .await
    }

    pub async fn with_queue_and_channel(
        &self,
        queue: QueueSettings,
        channel: ChannelSettings,
    ) -> pugbot::repositories::QueueRow {
        self.store
            .enable_channel(self.guild, self.channel, &channel)
            .await
            .expect("enable channel");
        self.store
            .create_queue(self.guild, self.channel, &queue)
            .await
            .expect("create queue");
        self.store
            .require_queue(self.channel)
            .await
            .expect("load queue")
    }

    pub fn now(&self) -> chrono::DateTime<chrono::Utc> {
        self.clock.now()
    }
}

fn test_config(url: &str) -> AppConfig {
    use std::collections::HashMap;
    let mut env: HashMap<String, String> = HashMap::new();
    env.insert("PUGBOT_DEBUG_DISCORD_TOKEN".into(), "test-token".into());
    env.insert("PUGBOT_DEBUG_APPLICATION_ID".into(), "1".into());
    env.insert("PUGBOT_DEBUG_DATABASE_URL".into(), url.to_string());
    env.insert(
        "PUGBOT_DEBUG_GUILD_ALLOWLIST".into(),
        "900000000000000001".into(),
    );
    AppConfig::load(Mode::Debug, &env).expect("test configuration is valid")
}
