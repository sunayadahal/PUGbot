//! Runtime configuration, and the hard separation between debug and
//! production.
//!
//! The two modes read entirely different environment variables
//! (`PUGBOT_DEBUG_*` versus `PUGBOT_PRODUCTION_*`). There is deliberately no
//! shared fallback: a missing debug token is an error, never a silent reach
//! for the production one.

use std::collections::HashMap;
use std::fmt;
use std::net::SocketAddr;
use std::path::Path;
use std::str::FromStr;

use serde::Serialize;
use thiserror::Error;

use crate::domain::ids::{GuildId, UserId};

/// Which mode the process is running in.
///
/// Chosen by an explicit command-line argument with no default, so a mistyped
/// command cannot start against production credentials.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// Local development and integration testing against a dedicated test bot,
    /// a dedicated database, and an enforced guild allowlist.
    Debug,
    /// Live operation against the production bot and database.
    Production,
}

impl Mode {
    /// The lowercase name used in logs, metrics, audit rows, and job leases.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Mode::Debug => "debug",
            Mode::Production => "production",
        }
    }

    /// The environment-variable prefix this mode reads.
    ///
    /// Nothing else in the process reads the other mode's prefix, which is what
    /// makes a fallback between modes impossible rather than merely discouraged.
    #[must_use]
    pub const fn prefix(self) -> &'static str {
        match self {
            Mode::Debug => "PUGBOT_DEBUG_",
            Mode::Production => "PUGBOT_PRODUCTION_",
        }
    }

    /// The mode this one is *not*. Used by the cross-check that refuses to
    /// start when both modes share a credential.
    #[must_use]
    pub const fn other(self) -> Mode {
        match self {
            Mode::Debug => Mode::Production,
            Mode::Production => Mode::Debug,
        }
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A string that must never reach a log line.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

// `Debug` and `Display` are implemented by hand below so a secret cannot reach
// a log line through the derive.

impl Secret {
    /// Wraps a value that must not be logged.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Yields the underlying value.
    ///
    /// Deliberately explicit: every call site is a place to check that the
    /// secret is being handed to a client library, not to a formatter.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[redacted]")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[redacted]")
    }
}

/// A configuration value that is missing, unparseable, or unsafe.
#[derive(Debug, Error)]
pub enum ConfigError {
    // Each variant names the offending key so an operator can fix it without
    // reading the source.
    /// A required variable is unset or blank.
    #[error("{mode} mode requires {prefix}{key} to be set")]
    Missing {
        /// The mode whose configuration was being loaded.
        mode: Mode,
        /// That mode's variable prefix.
        prefix: &'static str,
        /// The unprefixed variable name.
        key: &'static str,
    },
    /// A variable is set but cannot be parsed.
    #[error("{prefix}{key} is not a valid {expected}: {value}")]
    Invalid {
        /// The mode's variable prefix.
        prefix: &'static str,
        /// The unprefixed variable name.
        key: &'static str,
        /// What form was expected, phrased for an operator.
        expected: &'static str,
        /// The value that was rejected.
        value: String,
    },
    /// Debug and production are not properly separated. Carries the specific
    /// violation.
    #[error("mode isolation violated: {0}")]
    ModeIsolation(String),
    /// The `--env-file` could not be read.
    #[error("could not read env file {path}: {source}")]
    EnvFile {
        /// The path that was attempted.
        path: String,
        /// The underlying I/O failure.
        source: std::io::Error,
    },
}

/// Fully validated runtime configuration.
///
/// Constructing one of these is proof that the mode-isolation checks passed.
#[derive(Debug, Clone)]
pub struct AppConfig {
    /// Which mode this process is running in.
    pub mode: Mode,
    /// The bot token for this mode's Discord application.
    pub discord_token: Secret,
    /// This mode's Discord application ID.
    pub application_id: u64,
    /// Users granted owner-level access.
    pub owner_ids: Vec<UserId>,
    /// This mode's database connection string.
    pub database_url: Secret,
    /// Upper bound on the connection pool.
    pub database_max_connections: u32,
    /// A `tracing` filter directive, such as `pugbot=debug,info`.
    pub log_level: String,
    /// Guilds the bot will serve. Required and enforced in debug mode;
    /// optional in production, where an empty list means "any guild".
    pub guild_allowlist: Vec<GuildId>,
    /// Address for the health, readiness, and metrics endpoints. `None`
    /// disables the HTTP server entirely.
    pub health_bind: Option<SocketAddr>,
    /// Externally reachable base URL, for links in messages.
    pub public_url: Option<String>,
}

/// The environment as configuration loading sees it.
///
/// Abstracted so tests can supply a fixed map instead of mutating the real
/// process environment, which is global and racy under a parallel test runner.
pub trait EnvSource {
    /// The value of `key`, or `None` if it is unset.
    fn get(&self, key: &str) -> Option<String>;
}

/// Reads the real process environment.
/// Reads from the real process environment. See [`EnvSource`].
#[derive(Debug, Clone, Copy, Default)]
pub struct ProcessEnv;

impl EnvSource for ProcessEnv {
    fn get(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }
}

impl EnvSource for HashMap<String, String> {
    fn get(&self, key: &str) -> Option<String> {
        HashMap::get(self, key).cloned()
    }
}

impl AppConfig {
    /// Loads and validates configuration for `mode` from the process
    /// environment.
    ///
    /// # Errors
    ///
    /// See [`AppConfig::load`].
    pub fn from_process_env(mode: Mode) -> Result<Self, ConfigError> {
        Self::load(mode, &ProcessEnv)
    }

    /// Loads and validates configuration for `mode` from an arbitrary
    /// environment.
    ///
    /// Only variables carrying this mode's prefix are read. The other mode's
    /// variables are consulted for one purpose only: to refuse to start when
    /// the two modes share a token, a database, or a guild.
    ///
    /// # Errors
    ///
    /// * [`ConfigError::Missing`] — a required variable is unset or blank.
    /// * [`ConfigError::Invalid`] — a variable is set but unparseable.
    /// * [`ConfigError::ModeIsolation`] — debug has no guild allowlist, or the
    ///   two modes share a credential or a guild.
    pub fn load(mode: Mode, env: &dyn EnvSource) -> Result<Self, ConfigError> {
        let prefix = mode.prefix();
        let required = |key: &'static str| -> Result<String, ConfigError> {
            env.get(&format!("{prefix}{key}"))
                .filter(|value| !value.trim().is_empty())
                .ok_or(ConfigError::Missing { mode, prefix, key })
        };
        let optional = |key: &str| -> Option<String> {
            env.get(&format!("{prefix}{key}"))
                .filter(|value| !value.trim().is_empty())
        };

        let discord_token = required("DISCORD_TOKEN")?;
        let application_id_raw = required("APPLICATION_ID")?;
        let application_id =
            application_id_raw
                .trim()
                .parse::<u64>()
                .map_err(|_| ConfigError::Invalid {
                    prefix,
                    key: "APPLICATION_ID",
                    expected: "Discord snowflake",
                    value: application_id_raw.clone(),
                })?;
        let database_url = required("DATABASE_URL")?;

        let owner_ids = parse_id_list(optional("OWNER_IDS").as_deref(), prefix, "OWNER_IDS")?
            .into_iter()
            .map(UserId)
            .collect();
        let guild_allowlist = parse_id_list(
            optional("GUILD_ALLOWLIST").as_deref(),
            prefix,
            "GUILD_ALLOWLIST",
        )?
        .into_iter()
        .map(GuildId)
        .collect::<Vec<_>>();

        let database_max_connections = match optional("DATABASE_MAX_CONNECTIONS") {
            Some(raw) => raw
                .trim()
                .parse::<u32>()
                .map_err(|_| ConfigError::Invalid {
                    prefix,
                    key: "DATABASE_MAX_CONNECTIONS",
                    expected: "positive integer",
                    value: raw.clone(),
                })?,
            None => 10,
        };

        let health_bind = match optional("HEALTH_BIND") {
            Some(raw) => {
                Some(
                    SocketAddr::from_str(raw.trim()).map_err(|_| ConfigError::Invalid {
                        prefix,
                        key: "HEALTH_BIND",
                        expected: "socket address such as 0.0.0.0:8080",
                        value: raw.clone(),
                    })?,
                )
            }
            None => None,
        };

        let log_level = optional("LOG_LEVEL").unwrap_or_else(|| match mode {
            // Debug mode is verbose by default; that is part of what the mode
            // is for, not something the operator should have to remember.
            Mode::Debug => "pugbot=debug,info".to_string(),
            Mode::Production => "pugbot=info,warn".to_string(),
        });

        let config = AppConfig {
            mode,
            discord_token: Secret::new(discord_token),
            application_id,
            owner_ids,
            database_url: Secret::new(database_url),
            database_max_connections,
            log_level,
            guild_allowlist,
            health_bind,
            public_url: optional("PUBLIC_URL"),
        };
        config.enforce_mode_isolation(env)?;
        Ok(config)
    }

    /// Cross-checks that this mode's credentials are not the other mode's.
    ///
    /// This runs before any connection is opened. It is the check that stops a
    /// debug run from pointing at the production guild or database because
    /// somebody copied an environment file.
    fn enforce_mode_isolation(&self, env: &dyn EnvSource) -> Result<(), ConfigError> {
        if self.mode == Mode::Debug && self.guild_allowlist.is_empty() {
            return Err(ConfigError::ModeIsolation(
                "debug mode requires PUGBOT_DEBUG_GUILD_ALLOWLIST so interactions from \
                 unapproved guilds can be rejected"
                    .to_string(),
            ));
        }

        let other = self.mode.other();
        let other_prefix = other.prefix();

        if let Some(other_token) = env.get(&format!("{other_prefix}DISCORD_TOKEN")) {
            if !other_token.trim().is_empty()
                && other_token.trim() == self.discord_token.expose().trim()
            {
                return Err(ConfigError::ModeIsolation(format!(
                    "the {} Discord token is identical to the {other} token; each mode \
                     needs its own Discord application",
                    self.mode
                )));
            }
        }

        if let Some(other_db) = env.get(&format!("{other_prefix}DATABASE_URL")) {
            if !other_db.trim().is_empty() && other_db.trim() == self.database_url.expose().trim() {
                return Err(ConfigError::ModeIsolation(format!(
                    "the {} database URL is identical to the {other} database URL; each \
                     mode needs its own database or schema",
                    self.mode
                )));
            }
        }

        if self.mode == Mode::Debug {
            if let Some(production_guilds) =
                env.get(&format!("{}GUILD_ALLOWLIST", Mode::Production.prefix()))
            {
                let production_ids =
                    parse_id_list(Some(&production_guilds), other_prefix, "GUILD_ALLOWLIST")
                        .unwrap_or_default();
                if let Some(overlap) = self
                    .guild_allowlist
                    .iter()
                    .find(|guild| production_ids.contains(&guild.get()))
                {
                    return Err(ConfigError::ModeIsolation(format!(
                        "guild {overlap} appears in both the debug and production \
                         allowlists; debug must not touch production guilds"
                    )));
                }
            }
        }

        Ok(())
    }

    /// Whether this guild may be served at all.
    ///
    /// Debug always enforces the allowlist — that is the point of it. In
    /// production an empty list means no restriction.
    #[must_use]
    pub fn guild_allowed(&self, guild: GuildId) -> bool {
        match self.mode {
            // Debug always enforces the allowlist; that is the point of it.
            Mode::Debug => self.guild_allowlist.contains(&guild),
            // Production treats an empty list as "no restriction".
            Mode::Production => {
                self.guild_allowlist.is_empty() || self.guild_allowlist.contains(&guild)
            }
        }
    }

    /// Whether this user is a configured bot owner.
    #[must_use]
    pub fn is_owner(&self, user: UserId) -> bool {
        self.owner_ids.contains(&user)
    }

    /// Whether this process is running in debug mode.
    #[must_use]
    pub fn is_debug(&self) -> bool {
        self.mode == Mode::Debug
    }

    /// A one-line summary safe to print at startup and attach to logs.
    ///
    /// Carries the mode, application ID, database host and name, allowlisted
    /// guilds, owner count, and health binding — and no credentials. Printed
    /// before anything connects, so an operator can stop a run that is pointed
    /// at the wrong place.
    #[must_use]
    pub fn startup_summary(&self) -> String {
        let guilds = if self.guild_allowlist.is_empty() {
            "<any>".to_string()
        } else {
            self.guild_allowlist
                .iter()
                .map(GuildId::to_string)
                .collect::<Vec<_>>()
                .join(",")
        };
        format!(
            "mode={} application_id={} database={} guilds=[{}] owners={} health={}",
            self.mode,
            self.application_id,
            redact_database_url(self.database_url.expose()),
            guilds,
            self.owner_ids.len(),
            self.health_bind
                .map_or_else(|| "disabled".to_string(), |addr| addr.to_string())
        )
    }
}

fn parse_id_list(
    raw: Option<&str>,
    prefix: &'static str,
    key: &'static str,
) -> Result<Vec<i64>, ConfigError> {
    let Some(raw) = raw else {
        return Ok(Vec::new());
    };
    raw.split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(|part| {
            part.parse::<i64>().map_err(|_| ConfigError::Invalid {
                prefix,
                key,
                expected: "comma-separated Discord snowflakes",
                value: part.to_string(),
            })
        })
        .collect()
}

/// Strips credentials from a database URL so it can be logged.
///
/// Both the userinfo component and any query string are dropped, since a
/// password can hide in either.
///
/// `postgres://user:pw@host:5432/db` becomes `postgres://host:5432/db`.
pub fn redact_database_url(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return "<malformed url>".to_string();
    };
    let host_and_path = match rest.rsplit_once('@') {
        Some((_credentials, tail)) => tail,
        None => rest,
    };
    // Query strings can carry passwords too.
    let host_and_path = host_and_path
        .split_once('?')
        .map_or(host_and_path, |(head, _)| head);
    format!("{scheme}://{host_and_path}")
}

/// Loads `KEY=VALUE` lines from a file into the process environment.
///
/// Returns how many variables were set.
///
/// # Errors
///
/// Returns [`ConfigError::EnvFile`] if the file cannot be read.
///
/// Existing variables win, so an explicit environment always overrides a file.
/// Comments (`#`) and blank lines are ignored, and values may be quoted.
pub fn load_env_file(path: &Path) -> Result<usize, ConfigError> {
    let contents = std::fs::read_to_string(path).map_err(|source| ConfigError::EnvFile {
        path: path.display().to_string(),
        source,
    })?;
    let mut loaded = 0;
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        let value = value
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
            .unwrap_or(value);
        if std::env::var_os(key).is_none() {
            // SAFETY-adjacent note: this runs during startup, before any
            // thread that reads the environment concurrently is spawned.
            std::env::set_var(key, value);
            loaded += 1;
        }
    }
    Ok(loaded)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    fn debug_env() -> HashMap<String, String> {
        env(&[
            ("PUGBOT_DEBUG_DISCORD_TOKEN", "debug-token"),
            ("PUGBOT_DEBUG_APPLICATION_ID", "111"),
            (
                "PUGBOT_DEBUG_DATABASE_URL",
                "postgres://dev:pw@localhost/pugbot_debug",
            ),
            ("PUGBOT_DEBUG_GUILD_ALLOWLIST", "500,501"),
        ])
    }

    fn production_env() -> HashMap<String, String> {
        env(&[
            ("PUGBOT_PRODUCTION_DISCORD_TOKEN", "prod-token"),
            ("PUGBOT_PRODUCTION_APPLICATION_ID", "222"),
            (
                "PUGBOT_PRODUCTION_DATABASE_URL",
                "postgres://app:pw@db.internal/pugbot",
            ),
        ])
    }

    #[test]
    fn debug_config_loads_from_its_own_prefix() {
        let config = AppConfig::load(Mode::Debug, &debug_env()).unwrap();
        assert_eq!(config.mode, Mode::Debug);
        assert_eq!(config.application_id, 111);
        assert_eq!(config.discord_token.expose(), "debug-token");
        assert_eq!(config.guild_allowlist, vec![GuildId(500), GuildId(501)]);
    }

    #[test]
    fn production_config_loads_from_its_own_prefix() {
        let config = AppConfig::load(Mode::Production, &production_env()).unwrap();
        assert_eq!(config.application_id, 222);
        assert!(config.guild_allowlist.is_empty());
    }

    #[test]
    fn debug_startup_fails_when_only_production_credentials_are_present() {
        let error = AppConfig::load(Mode::Debug, &production_env()).unwrap_err();
        assert!(
            matches!(
                error,
                ConfigError::Missing {
                    key: "DISCORD_TOKEN",
                    ..
                }
            ),
            "expected a missing-token error, got {error}"
        );
    }

    #[test]
    fn production_startup_fails_when_only_debug_credentials_are_present() {
        let error = AppConfig::load(Mode::Production, &debug_env()).unwrap_err();
        assert!(matches!(
            error,
            ConfigError::Missing {
                key: "DISCORD_TOKEN",
                ..
            }
        ));
    }

    #[test]
    fn neither_mode_falls_back_to_the_other_for_any_single_value() {
        // Debug is fully configured except for the database; production has
        // one. The load must fail rather than borrow it.
        let mut combined = debug_env();
        combined.remove("PUGBOT_DEBUG_DATABASE_URL");
        combined.extend(production_env());
        let error = AppConfig::load(Mode::Debug, &combined).unwrap_err();
        assert!(matches!(
            error,
            ConfigError::Missing {
                key: "DATABASE_URL",
                ..
            }
        ));
    }

    #[test]
    fn debug_requires_a_guild_allowlist() {
        let mut broken = debug_env();
        broken.remove("PUGBOT_DEBUG_GUILD_ALLOWLIST");
        let error = AppConfig::load(Mode::Debug, &broken).unwrap_err();
        assert!(matches!(error, ConfigError::ModeIsolation(_)), "{error}");
    }

    #[test]
    fn a_shared_token_between_modes_is_rejected() {
        let mut combined = debug_env();
        combined.extend(production_env());
        combined.insert(
            "PUGBOT_PRODUCTION_DISCORD_TOKEN".into(),
            "debug-token".into(),
        );
        let error = AppConfig::load(Mode::Debug, &combined).unwrap_err();
        assert!(matches!(error, ConfigError::ModeIsolation(_)), "{error}");
    }

    #[test]
    fn a_shared_database_between_modes_is_rejected() {
        let mut combined = debug_env();
        combined.extend(production_env());
        combined.insert(
            "PUGBOT_PRODUCTION_DATABASE_URL".into(),
            "postgres://dev:pw@localhost/pugbot_debug".into(),
        );
        let error = AppConfig::load(Mode::Debug, &combined).unwrap_err();
        assert!(matches!(error, ConfigError::ModeIsolation(_)), "{error}");
    }

    #[test]
    fn a_guild_in_both_allowlists_is_rejected_for_debug() {
        let mut combined = debug_env();
        combined.extend(production_env());
        combined.insert("PUGBOT_PRODUCTION_GUILD_ALLOWLIST".into(), "900,501".into());
        let error = AppConfig::load(Mode::Debug, &combined).unwrap_err();
        assert!(
            matches!(&error, ConfigError::ModeIsolation(m) if m.contains("501")),
            "{error}"
        );
    }

    #[test]
    fn a_correctly_separated_pair_of_configurations_loads_in_both_modes() {
        let mut combined = debug_env();
        combined.extend(production_env());
        AppConfig::load(Mode::Debug, &combined).unwrap();
        AppConfig::load(Mode::Production, &combined).unwrap();
    }

    #[test]
    fn debug_rejects_guilds_outside_the_allowlist() {
        let config = AppConfig::load(Mode::Debug, &debug_env()).unwrap();
        assert!(config.guild_allowed(GuildId(500)));
        assert!(!config.guild_allowed(GuildId(999)));
    }

    #[test]
    fn production_without_an_allowlist_serves_any_guild() {
        let config = AppConfig::load(Mode::Production, &production_env()).unwrap();
        assert!(config.guild_allowed(GuildId(999)));

        let mut restricted = production_env();
        restricted.insert("PUGBOT_PRODUCTION_GUILD_ALLOWLIST".into(), "42".into());
        let config = AppConfig::load(Mode::Production, &restricted).unwrap();
        assert!(config.guild_allowed(GuildId(42)));
        assert!(!config.guild_allowed(GuildId(43)));
    }

    #[test]
    fn malformed_ids_are_reported_with_their_key() {
        let mut broken = debug_env();
        broken.insert("PUGBOT_DEBUG_APPLICATION_ID".into(), "not-a-number".into());
        assert!(matches!(
            AppConfig::load(Mode::Debug, &broken).unwrap_err(),
            ConfigError::Invalid {
                key: "APPLICATION_ID",
                ..
            }
        ));

        let mut broken = debug_env();
        broken.insert("PUGBOT_DEBUG_OWNER_IDS".into(), "1,2,three".into());
        assert!(matches!(
            AppConfig::load(Mode::Debug, &broken).unwrap_err(),
            ConfigError::Invalid {
                key: "OWNER_IDS",
                ..
            }
        ));
    }

    #[test]
    fn blank_values_count_as_missing() {
        let mut broken = debug_env();
        broken.insert("PUGBOT_DEBUG_DISCORD_TOKEN".into(), "   ".into());
        assert!(matches!(
            AppConfig::load(Mode::Debug, &broken).unwrap_err(),
            ConfigError::Missing { .. }
        ));
    }

    #[test]
    fn log_defaults_are_verbose_in_debug_and_quiet_in_production() {
        assert!(AppConfig::load(Mode::Debug, &debug_env())
            .unwrap()
            .log_level
            .contains("debug"));
        assert!(AppConfig::load(Mode::Production, &production_env())
            .unwrap()
            .log_level
            .contains("info"));
    }

    #[test]
    fn secrets_are_redacted_in_debug_and_display_output() {
        let secret = Secret::new("hunter2");
        assert_eq!(format!("{secret:?}"), "[redacted]");
        assert_eq!(format!("{secret}"), "[redacted]");
        assert_eq!(secret.expose(), "hunter2");

        let config = AppConfig::load(Mode::Debug, &debug_env()).unwrap();
        let rendered = format!("{config:?}");
        assert!(!rendered.contains("debug-token"), "{rendered}");
        assert!(!rendered.contains("pw@"), "{rendered}");
    }

    #[test]
    fn the_startup_summary_carries_no_credentials() {
        let config = AppConfig::load(Mode::Debug, &debug_env()).unwrap();
        let summary = config.startup_summary();
        assert!(summary.contains("mode=debug"));
        assert!(summary.contains("application_id=111"));
        assert!(summary.contains("localhost/pugbot_debug"));
        assert!(summary.contains("500,501"));
        assert!(!summary.contains("dev:pw"), "{summary}");
    }

    #[test]
    fn database_urls_are_redacted_for_logging() {
        assert_eq!(
            redact_database_url("postgres://user:secret@db.host:5432/pugbot"),
            "postgres://db.host:5432/pugbot"
        );
        assert_eq!(
            redact_database_url("postgres://localhost/pugbot?password=secret"),
            "postgres://localhost/pugbot"
        );
        assert_eq!(redact_database_url("nonsense"), "<malformed url>");
    }

    #[test]
    fn mode_prefixes_never_collide() {
        assert_ne!(Mode::Debug.prefix(), Mode::Production.prefix());
        assert_eq!(Mode::Debug.other(), Mode::Production);
        assert_eq!(Mode::Production.other(), Mode::Debug);
    }
}
