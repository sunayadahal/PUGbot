#![warn(missing_docs)]
#![warn(clippy::missing_panics_doc)]

//! PUGbot entry point.
//!
//! This binary is a thin shell around the [`pugbot`] library: it parses the
//! command line, loads and validates configuration, installs logging, and wires
//! the services together. Every rule lives in the library.
//!
//! # Startup sequence
//!
//! 1. Parse arguments. The mode is required — there is no default, so a
//!    mistyped command cannot start against production credentials.
//! 2. Optionally load an environment file. Existing variables always win.
//! 3. Load configuration for the selected mode and run the mode-isolation
//!    cross-checks. Nothing has connected at this point.
//! 4. Install logging and print the startup banner, so an operator can stop a
//!    run pointed at the wrong place before it does anything.
//! 5. Connect, migrate, recover in-flight matches, start the background jobs
//!    and the health endpoint, then connect to Discord.
//!
//! # Exit codes
//!
//! Zero on success. Any startup failure returns non-zero with a message on
//! standard error describing which variable or check failed.

use std::path::PathBuf;
use std::sync::Arc;

#[cfg(feature = "debug-tools")]
use anyhow::bail;
use anyhow::Context as _;
use clap::{Parser, Subcommand};

use pugbot::config::{AppConfig, Mode};
use pugbot::discord::DiscordNotifier;
use pugbot::domain::clock::SystemClock;
use pugbot::repositories::Store;
use pugbot::services::AppContext;
use pugbot::{discord, jobs, observability};

#[derive(Debug, Parser)]
#[command(name = "pugbot", version, about = "Discord pickup game bot")]
/// Command-line arguments.
struct Cli {
    /// Which mode to run in. Required: production is never the default.
    #[arg(long, value_enum)]
    mode: Mode,

    /// Optional file of KEY=VALUE lines loaded before configuration is read.
    /// Existing environment variables always win.
    #[arg(long)]
    env_file: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
/// What the process should do. Defaults to [`Command::Run`].
enum Command {
    /// Connect to Discord and serve (the default).
    Run,
    /// Apply database migrations and exit.
    Migrate,
    /// Validate configuration and connectivity, then exit.
    Check,
    /// Delete every row in the debug database. Debug mode only, and only in
    /// builds compiled with the `debug-tools` feature.
    #[cfg(feature = "debug-tools")]
    DebugReset {
        /// Required acknowledgement, so this cannot be run by accident.
        #[arg(long)]
        yes_delete_everything: bool,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    if let Some(path) = &cli.env_file {
        let loaded = pugbot::config::load_env_file(path)
            .with_context(|| format!("loading {}", path.display()))?;
        eprintln!("loaded {loaded} variables from {}", path.display());
    }

    let config = AppConfig::from_process_env(cli.mode)
        .with_context(|| format!("loading {} configuration", cli.mode))?;

    observability::init_tracing(&config);
    // Printed before anything connects, so an operator can stop a run that is
    // pointed at the wrong place.
    tracing::info!(summary = %config.startup_summary(), "starting PUGbot");

    match cli.command.unwrap_or(Command::Run) {
        Command::Migrate => migrate(&config).await,
        Command::Check => check(&config).await,
        Command::Run => run(config).await,
        #[cfg(feature = "debug-tools")]
        Command::DebugReset {
            yes_delete_everything,
        } => debug_reset(&config, yes_delete_everything).await,
    }
}

/// Applies pending migrations and exits.
///
/// # Errors
///
/// Returns an error if the database is unreachable or a migration fails.
async fn migrate(config: &AppConfig) -> anyhow::Result<()> {
    let store = Store::connect(config).await?;
    store.migrate().await?;
    tracing::info!("migrations applied");
    Ok(())
}

/// Validates configuration and database connectivity, then exits.
///
/// Intended for a deployment smoke test: it exercises everything except the
/// Discord connection.
///
/// # Errors
///
/// Returns an error if the database is unreachable.
async fn check(config: &AppConfig) -> anyhow::Result<()> {
    let store = Store::connect(config).await?;
    store.ping().await.context("database is not reachable")?;
    tracing::info!(
        mode = config.mode.as_str(),
        guilds = config.guild_allowlist.len(),
        "configuration is valid and the database is reachable"
    );
    Ok(())
}

/// Runs the bot until the process is interrupted.
///
/// # Errors
///
/// Returns an error if migrations fail, the Discord client cannot be built, or
/// the gateway connection ends unexpectedly.
async fn run(config: AppConfig) -> anyhow::Result<()> {
    let store = Arc::new(Store::connect(&config).await?);
    store.migrate().await.context("applying migrations")?;

    let config = Arc::new(config);
    // A standalone HTTP client so the notifier exists before the gateway
    // connects; the two share nothing but the token.
    let http = Arc::new(serenity::http::Http::new(config.discord_token.expose()));
    let notifier = Arc::new(DiscordNotifier::new(http, Arc::clone(&store)));

    let app = AppContext::new(
        Arc::clone(&store),
        Arc::clone(&config),
        Arc::new(SystemClock),
        notifier,
    );

    jobs::recover_on_startup(&app).await;

    if let Some(bind) = config.health_bind {
        let store = Arc::clone(&store);
        let config = Arc::clone(&config);
        tokio::spawn(async move {
            if let Err(error) = observability::serve_health(bind, store, &config).await {
                tracing::error!(%error, "health endpoint stopped");
            }
        });
    }

    tokio::spawn(jobs::run_timers(app.clone()));
    tokio::spawn(jobs::run_decay(app.clone()));

    let mut client = discord::build_client(app, &config).await?;
    let shard_manager = client.shard_manager.clone();
    tokio::spawn(async move {
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::error!(%error, "could not listen for shutdown");
            return;
        }
        tracing::info!("shutting down");
        shard_manager.shutdown_all().await;
    });

    client.start().await.context("Discord client stopped")?;
    Ok(())
}

/// Wipes the debug database.
///
/// Guarded three ways: the `debug-tools` feature must be compiled in, the mode
/// must be debug, and the operator must pass an explicit acknowledgement. The
/// mode check is redundant given the feature gate, and deliberately so — a
/// destructive tool should not rely on a single guard.
///
/// # Errors
///
/// Returns an error in production mode, without the acknowledgement flag, or if
/// a truncation fails.
#[cfg(feature = "debug-tools")]
async fn debug_reset(config: &AppConfig, acknowledged: bool) -> anyhow::Result<()> {
    if config.mode != Mode::Debug {
        bail!("debug-reset refuses to run in {} mode", config.mode);
    }
    if !acknowledged {
        bail!("debug-reset requires --yes-delete-everything");
    }

    let store = Store::connect(config).await?;
    // Ordered so foreign keys never block the truncate.
    for table in [
        "audit_events",
        "rating_history",
        "match_reports",
        "map_votes",
        "draft_picks",
        "match_players",
        "matches",
        "queue_members",
        "queues",
        "channel_players",
        "player_phrases",
        "subscriptions",
        "queue_bans",
        "job_locks",
        "users",
        "channel_configs",
        "guilds",
    ] {
        sqlx::query(&format!("TRUNCATE TABLE {table} CASCADE"))
            .execute(store.pool())
            .await
            .with_context(|| format!("truncating {table}"))?;
    }
    tracing::warn!(mode = config.mode.as_str(), "debug database reset");
    Ok(())
}
