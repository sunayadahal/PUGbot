//! Queue bans, join phrases, subscriptions, and personal preferences.

use chrono::{DateTime, Duration, Utc};

use super::{humanize_seconds, AppContext};
use crate::domain::ids::{ChannelId, GuildId, RoleId, UserId};
use crate::domain::permissions::{Actor, PermissionLevel};
use crate::error::{ServiceError, ServiceResult};
use crate::repositories::moderation::QueueBanRow;
use crate::repositories::{ChannelConfigRow, UserPrefsRow};
use crate::services::queue_svc::QueueService;

/// Upper bound on a single queue ban, so a typo cannot ban somebody for a
/// century. A moderator who wants longer can re-issue it.
const MAX_BAN_SECONDS: i64 = 365 * 24 * 3600;

/// Queue bans, join phrases, subscriptions, and personal preferences.
#[derive(Debug, Clone)]
pub struct ModerationService {
    ctx: AppContext,
}

impl ModerationService {
    /// Wraps the shared application context.
    #[must_use]
    pub fn new(ctx: AppContext) -> Self {
        Self { ctx }
    }

    /// Issues a timed guild-wide queue ban and drops the player from every
    /// queue they are sitting in.
    ///
    /// The duration is capped at one year, so a typo cannot ban somebody for a
    /// century; a moderator who wants longer can re-issue it.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Forbidden`] unless the caller is a moderator,
    /// [`ServiceError::Rejected`] for a non-positive duration, or
    /// [`ServiceError::Database`].
    pub async fn ban(
        &self,
        channel: &ChannelConfigRow,
        actor: &Actor,
        user: UserId,
        duration_seconds: i64,
        reason: Option<&str>,
    ) -> ServiceResult<QueueBanRow> {
        self.ctx
            .require_permission(actor, &channel.settings, PermissionLevel::Moderator)?;
        if duration_seconds <= 0 {
            return Err(ServiceError::Rejected(
                "a ban needs a positive duration".to_string(),
            ));
        }
        let duration_seconds = duration_seconds.min(MAX_BAN_SECONDS);
        let now = self.ctx.now();
        let expires_at = now + Duration::seconds(duration_seconds);

        self.ctx
            .store
            .add_queue_ban(channel.guild, user, actor.user, reason, expires_at)
            .await?;
        // A banned player should not stay queued somewhere else in the guild.
        QueueService::new(self.ctx.clone())
            .remove_everywhere(user)
            .await?;

        self.ctx
            .audit(
                Some(channel.guild),
                Some(channel.channel),
                Some(actor.user),
                "moderation.ban",
                Some(&user.to_string()),
                serde_json::json!({
                    "duration": humanize_seconds(duration_seconds),
                    "reason": reason,
                }),
            )
            .await;

        let bans = self.ctx.store.active_bans(channel.guild, now).await?;
        bans.into_iter()
            .find(|ban| ban.user == user)
            .ok_or_else(|| ServiceError::Other(anyhow::anyhow!("ban vanished after insert")))
    }

    /// Lifts every active ban on a player. Returns how many were lifted.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Forbidden`] unless the caller is a moderator,
    /// [`ServiceError::Rejected`] if they had no active ban, or
    /// [`ServiceError::Database`].
    pub async fn unban(
        &self,
        channel: &ChannelConfigRow,
        actor: &Actor,
        user: UserId,
    ) -> ServiceResult<u64> {
        self.ctx
            .require_permission(actor, &channel.settings, PermissionLevel::Moderator)?;
        let released = self
            .ctx
            .store
            .release_bans(channel.guild, user, actor.user)
            .await?;
        if released == 0 {
            return Err(ServiceError::Rejected(
                "that player has no active queue ban".to_string(),
            ));
        }
        self.ctx
            .audit(
                Some(channel.guild),
                Some(channel.channel),
                Some(actor.user),
                "moderation.unban",
                Some(&user.to_string()),
                serde_json::json!({ "released": released }),
            )
            .await;
        Ok(released)
    }

    /// Every active queue ban in a guild.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`] if the query fails.
    pub async fn list_bans(&self, guild: GuildId) -> ServiceResult<Vec<QueueBanRow>> {
        self.ctx.store.active_bans(guild, self.ctx.now()).await
    }

    // ---------------------------------------------------------- join phrases

    /// Adds a custom phrase shown when a player joins the queue.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Forbidden`] unless the caller is a moderator,
    /// [`ServiceError::Rejected`] for an empty phrase or one over 200
    /// characters, or [`ServiceError::Database`].
    pub async fn add_phrase(
        &self,
        channel: &ChannelConfigRow,
        actor: &Actor,
        user: UserId,
        phrase: &str,
    ) -> ServiceResult<()> {
        self.ctx
            .require_permission(actor, &channel.settings, PermissionLevel::Moderator)?;
        let phrase = phrase.trim();
        if phrase.is_empty() {
            return Err(ServiceError::Rejected("the phrase is empty".to_string()));
        }
        if phrase.chars().count() > 200 {
            return Err(ServiceError::Rejected(
                "a join phrase must be 200 characters or fewer".to_string(),
            ));
        }
        self.ctx
            .store
            .add_phrase(channel.channel, user, phrase)
            .await?;
        self.ctx
            .audit(
                Some(channel.guild),
                Some(channel.channel),
                Some(actor.user),
                "moderation.phrase_added",
                Some(&user.to_string()),
                serde_json::json!({ "phrase": phrase }),
            )
            .await;
        Ok(())
    }

    /// Removes every join phrase a player has here. Returns how many.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Forbidden`] unless the caller is a moderator, or
    /// [`ServiceError::Database`].
    pub async fn clear_phrases(
        &self,
        channel: &ChannelConfigRow,
        actor: &Actor,
        user: UserId,
    ) -> ServiceResult<u64> {
        self.ctx
            .require_permission(actor, &channel.settings, PermissionLevel::Moderator)?;
        let removed = self.ctx.store.clear_phrases(channel.channel, user).await?;
        self.ctx
            .audit(
                Some(channel.guild),
                Some(channel.channel),
                Some(actor.user),
                "moderation.phrases_cleared",
                Some(&user.to_string()),
                serde_json::json!({ "removed": removed }),
            )
            .await;
        Ok(removed)
    }

    /// A player's join phrases in a channel.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`] if the query fails.
    pub async fn phrases(&self, channel: ChannelId, user: UserId) -> ServiceResult<Vec<String>> {
        self.ctx.store.phrases_for(channel, user).await
    }

    // --------------------------------------------------------- subscriptions

    /// `/subscribe`: opt in to promotion pings for this channel.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::NoQueue`] if the channel has no queue,
    /// [`ServiceError::Rejected`] if it has no promotion role configured, or
    /// [`ServiceError::Database`].
    pub async fn subscribe(
        &self,
        channel: &ChannelConfigRow,
        user: UserId,
    ) -> ServiceResult<RoleId> {
        let queue = self.ctx.store.require_queue(channel.channel).await?;
        let role = queue.settings.promotion_role_id.ok_or_else(|| {
            ServiceError::Rejected("this channel has no promotion role configured".to_string())
        })?;
        self.ctx
            .store
            .subscribe(channel.channel, user, role)
            .await?;
        Ok(role)
    }

    /// `/unsubscribe`: opt out of promotion pings.
    ///
    /// # Errors
    ///
    /// As [`ModerationService::subscribe`].
    pub async fn unsubscribe(
        &self,
        channel: &ChannelConfigRow,
        user: UserId,
    ) -> ServiceResult<RoleId> {
        let queue = self.ctx.store.require_queue(channel.channel).await?;
        let role = queue.settings.promotion_role_id.ok_or_else(|| {
            ServiceError::Rejected("this channel has no promotion role configured".to_string())
        })?;
        self.ctx
            .store
            .unsubscribe(channel.channel, user, role)
            .await?;
        Ok(role)
    }

    // ---------------------------------------------------- player preferences

    /// A player's personal preferences, defaulted if never set.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`] if the query fails.
    pub async fn preferences(&self, user: UserId) -> ServiceResult<UserPrefsRow> {
        self.ctx.store.user_prefs(user).await
    }

    /// `/switch-dms`: toggle match-start direct messages. Returns the new
    /// setting.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`] if a query fails.
    pub async fn toggle_dms(&self, user: UserId) -> ServiceResult<bool> {
        let mut prefs = self.ctx.store.user_prefs(user).await?;
        prefs.dm_on_start = !prefs.dm_on_start;
        self.ctx.store.save_user_prefs(&prefs).await?;
        Ok(prefs.dm_on_start)
    }

    /// `/expire-default`: set the player's own default queue expiry, clamped to
    /// the channel maximum. Zero means never expire.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`] if a query fails.
    pub async fn set_default_expiry(
        &self,
        channel: &ChannelConfigRow,
        user: UserId,
        seconds: Option<i64>,
    ) -> ServiceResult<Option<i64>> {
        let clamped = seconds.map(|value| {
            if value <= 0 {
                0
            } else {
                channel.settings.clamp_expiry(value)
            }
        });
        let mut prefs = self.ctx.store.user_prefs(user).await?;
        prefs.default_expiry_seconds = clamped;
        self.ctx.store.save_user_prefs(&prefs).await?;
        Ok(clamped)
    }

    /// `/expire`: override the expiry of the slot the player holds right now.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::NoQueue`],
    /// [`DomainError::NotQueued`](crate::error::DomainError::NotQueued) if they
    /// hold no slot, or [`ServiceError::Database`].
    pub async fn set_session_expiry(
        &self,
        channel: &ChannelConfigRow,
        user: UserId,
        seconds: i64,
    ) -> ServiceResult<Option<DateTime<Utc>>> {
        let queue = self.ctx.store.require_queue(channel.channel).await?;
        let now = self.ctx.now();
        let expires_at = if seconds <= 0 {
            None
        } else {
            Some(now + Duration::seconds(channel.settings.clamp_expiry(seconds)))
        };
        let updated = sqlx::query(
            "UPDATE queue_members SET expires_at = $3 WHERE queue_id = $1 AND user_id = $2",
        )
        .bind(queue.id.get())
        .bind(user.get())
        .bind(expires_at)
        .execute(self.ctx.store.pool())
        .await?;
        if updated.rows_affected() == 0 {
            return Err(crate::error::DomainError::NotQueued.into());
        }
        Ok(expires_at)
    }

    /// `/auto-ready`: arm a one-use automatic ready for the next match, clamped
    /// to the channel maximum. Returns the duration actually applied.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Rejected`] if the channel disables auto-ready,
    /// or [`ServiceError::Database`].
    pub async fn arm_auto_ready(
        &self,
        channel: &ChannelConfigRow,
        user: UserId,
        seconds: i64,
    ) -> ServiceResult<i64> {
        let clamped = channel.settings.clamp_auto_ready(seconds);
        if clamped == 0 {
            return Err(ServiceError::Rejected(
                "auto-ready is disabled in this channel".to_string(),
            ));
        }
        let mut prefs = self.ctx.store.user_prefs(user).await?;
        prefs.auto_ready_until = Some(self.ctx.now() + Duration::seconds(clamped));
        self.ctx.store.save_user_prefs(&prefs).await?;
        Ok(clamped)
    }

    /// `/allow-offline`: stay queued while offline for a while. Returns the
    /// duration actually applied.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Rejected`] if the channel forbids the opt-out,
    /// or [`ServiceError::Database`].
    pub async fn allow_offline(
        &self,
        channel: &ChannelConfigRow,
        user: UserId,
        seconds: i64,
    ) -> ServiceResult<i64> {
        if !channel.settings.allow_offline_opt_out {
            return Err(ServiceError::Rejected(
                "this channel does not allow staying queued while offline".to_string(),
            ));
        }
        let clamped = channel.settings.clamp_expiry(seconds);
        let mut prefs = self.ctx.store.user_prefs(user).await?;
        prefs.allow_offline_until = Some(self.ctx.now() + Duration::seconds(clamped));
        self.ctx.store.save_user_prefs(&prefs).await?;
        Ok(clamped)
    }
}
