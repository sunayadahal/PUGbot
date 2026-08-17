//! Application services: one module per use-case area.
//!
//! Services own transaction boundaries and translate between the pure domain
//! and the repositories. They never call Discord directly — commands return a
//! description of what happened and the Discord adapter renders it, while
//! background jobs push announcements through the [`Notifier`] trait. That
//! keeps every service testable without a gateway connection.

pub mod config_svc;
pub mod match_svc;
pub mod moderation_svc;
pub mod queue_svc;
pub mod rating_svc;

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::config::AppConfig;
use crate::domain::clock::Clock;
use crate::domain::ids::{ChannelId, GuildId, MatchId, UserId};
use crate::domain::permissions::{Actor, PermissionLevel};
use crate::domain::settings::ChannelSettings;
use crate::error::{ServiceError, ServiceResult};
use crate::localization::Locale;
use crate::repositories::Store;

/// Announcements a service wants delivered to Discord.
///
/// Interactive commands return these to the adapter; background jobs hand them
/// to a [`Notifier`].
/// Something a service wants said in a Discord channel.
///
/// Services return or emit these rather than calling Discord themselves, which
/// keeps them testable without a gateway connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Announcement {
    /// Plain text in the channel.
    Text(String),
    /// A match embed needs rendering or re-rendering.
    MatchUpdate(MatchId),
    /// A match has ended, with a short description of how.
    MatchFinished(MatchId, String),
}

/// Outbound Discord effects that do not originate from an interaction.
///
/// Background jobs and state transitions push through this trait; interactive
/// commands instead return a reply for the adapter to render.
#[async_trait]
pub trait Notifier: Send + Sync + std::fmt::Debug {
    /// Says something in a channel.
    ///
    /// Implementations must not fail the caller: a Discord outage should be
    /// logged, not propagated into a database transaction.
    async fn announce(&self, channel: ChannelId, announcement: Announcement);

    /// Sends a direct message.
    ///
    /// A player with closed DMs is a normal outcome, not an error.
    async fn direct_message(&self, user: UserId, text: String);
}

/// A notifier that drops everything, for tests and for the migration CLI.
/// A [`Notifier`] that discards everything. Used by tests and by the CLI
/// subcommands that never talk to Discord.
#[derive(Debug, Default, Clone, Copy)]
pub struct NullNotifier;

#[async_trait]
impl Notifier for NullNotifier {
    async fn announce(&self, _channel: ChannelId, _announcement: Announcement) {}
    async fn direct_message(&self, _user: UserId, _text: String) {}
}

/// Everything a service needs, cloned cheaply into each handler.
#[derive(Clone)]
pub struct AppContext {
    /// The database gateway.
    pub store: Arc<Store>,
    /// Validated runtime configuration, including the mode.
    pub config: Arc<AppConfig>,
    /// The clock. Injected so timer logic is testable.
    pub clock: Arc<dyn Clock>,
    /// Where announcements and DMs go.
    pub notifier: Arc<dyn Notifier>,
}

impl std::fmt::Debug for AppContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppContext")
            .field("mode", &self.config.mode)
            .finish_non_exhaustive()
    }
}

impl AppContext {
    /// Assembles a context from its parts.
    #[must_use]
    pub fn new(
        store: Arc<Store>,
        config: Arc<AppConfig>,
        clock: Arc<dyn Clock>,
        notifier: Arc<dyn Notifier>,
    ) -> Self {
        Self {
            store,
            config,
            clock,
            notifier,
        }
    }

    /// The current time, from the injected clock.
    #[must_use]
    pub fn now(&self) -> DateTime<Utc> {
        self.clock.now()
    }

    /// The running mode as a string, for logs, metrics, and audit rows.
    #[must_use]
    pub fn mode(&self) -> &'static str {
        self.config.mode.as_str()
    }

    /// Rejects interactions from guilds this mode is not allowed to serve.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Rejected`] naming the guild and the mode.
    ///
    /// In debug mode this is the guard that stops a test bot from acting on a
    /// production server it happens to be invited to.
    pub fn ensure_guild_allowed(&self, guild: GuildId) -> ServiceResult<()> {
        if self.config.guild_allowed(guild) {
            Ok(())
        } else {
            Err(ServiceError::Rejected(format!(
                "guild {guild} is not in the {} allowlist",
                self.config.mode
            )))
        }
    }

    /// Fails unless the process is running in debug mode.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::DebugOnly`] in production.
    pub fn ensure_debug_mode(&self) -> ServiceResult<()> {
        if self.config.is_debug() {
            Ok(())
        } else {
            Err(ServiceError::DebugOnly)
        }
    }

    /// Completes an actor by filling in bot-owner status from configuration.
    #[must_use]
    pub fn actor(&self, mut actor: Actor) -> Actor {
        actor.is_bot_owner = self.config.is_owner(actor.user);
        actor
    }

    /// Asserts that the actor meets a permission level in a channel.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Forbidden`] if they do not.
    pub fn require_permission(
        &self,
        actor: &Actor,
        settings: &ChannelSettings,
        required: PermissionLevel,
    ) -> ServiceResult<()> {
        if actor.can(settings, required) {
            Ok(())
        } else {
            Err(ServiceError::Forbidden)
        }
    }

    /// The locale to render this channel's messages in.
    #[must_use]
    pub fn locale(&self, settings: &ChannelSettings) -> Locale {
        Locale::resolve(&settings.locale)
    }

    /// Writes an audit row, tagged with the running mode.
    ///
    /// Audit failures are logged rather than propagated: losing the audit trail
    /// for one action is bad, but failing the moderator's command because the
    /// audit insert failed is worse.
    pub async fn audit(
        &self,
        guild: Option<GuildId>,
        channel: Option<ChannelId>,
        actor: Option<UserId>,
        action: &str,
        target: Option<&str>,
        data: serde_json::Value,
    ) {
        let event = crate::repositories::moderation::NewAuditEvent {
            mode: self.mode(),
            guild,
            channel,
            actor,
            action,
            target,
            data,
        };
        if let Err(error) = self.store.audit(&event).await {
            tracing::error!(%action, %error, "failed to write audit event");
        }
    }
}

/// Formats a duration for messages such as "banned for 2h 30m".
///
/// Seconds are shown only when the total is under an hour, so a long ban reads
/// as `7d 12h` rather than counting seconds nobody cares about.
pub fn humanize_seconds(total: i64) -> String {
    if total <= 0 {
        return "0s".to_string();
    }
    let days = total / 86_400;
    let hours = (total % 86_400) / 3_600;
    let minutes = (total % 3_600) / 60;
    let seconds = total % 60;
    let mut parts = Vec::new();
    if days > 0 {
        parts.push(format!("{days}d"));
    }
    if hours > 0 {
        parts.push(format!("{hours}h"));
    }
    if minutes > 0 {
        parts.push(format!("{minutes}m"));
    }
    if seconds > 0 && days == 0 && hours == 0 {
        parts.push(format!("{seconds}s"));
    }
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_read_naturally() {
        assert_eq!(humanize_seconds(0), "0s");
        assert_eq!(humanize_seconds(-5), "0s");
        assert_eq!(humanize_seconds(45), "45s");
        assert_eq!(humanize_seconds(90), "1m 30s");
        assert_eq!(humanize_seconds(3_600), "1h");
        assert_eq!(humanize_seconds(3_725), "1h 2m");
        assert_eq!(humanize_seconds(90_061), "1d 1h 1m");
    }
}
