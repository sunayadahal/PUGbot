//! Queue bans, join phrases, promotion subscriptions, and the audit log.

use chrono::{DateTime, Utc};
use sqlx::Row;

use super::Store;
use crate::domain::ids::{ChannelId, GuildId, RoleId, UserId};
use crate::error::ServiceResult;

/// A row of `queue_bans`: one timed, guild-wide ban.
#[derive(Debug, Clone)]
pub struct QueueBanRow {
    /// Primary key.
    pub id: i64,
    /// The guild the ban applies across.
    pub guild: GuildId,
    /// The banned player.
    pub user: UserId,
    /// The moderator who issued it.
    pub issuer: UserId,
    /// The stated reason, if one was given.
    pub reason: Option<String>,
    /// When the ban began.
    pub started_at: DateTime<Utc>,
    /// When it lapses on its own.
    pub expires_at: DateTime<Utc>,
    /// When it was lifted early, if it was.
    pub released_at: Option<DateTime<Utc>>,
    /// Who lifted it.
    pub released_by: Option<UserId>,
}

/// One row to append to the audit log.
#[derive(Debug, Clone)]
pub struct NewAuditEvent<'a> {
    /// The running mode, recorded on every row.
    pub mode: &'a str,
    /// The guild the action affected.
    pub guild: Option<GuildId>,
    /// The channel the action affected.
    pub channel: Option<ChannelId>,
    /// Who performed it, or `None` for an automatic action.
    pub actor: Option<UserId>,
    /// A dotted action name such as `moderation.ban`.
    pub action: &'a str,
    /// What was acted on, usually an id rendered as a string.
    pub target: Option<&'a str>,
    /// Structured detail, including before and after values for edits.
    pub data: serde_json::Value,
}

/// A row read back from `audit_events`.
#[derive(Debug, Clone)]
pub struct AuditEvent {
    /// The guild the action affected.
    pub guild: Option<GuildId>,
    /// The channel the action affected.
    pub channel: Option<ChannelId>,
    /// Who performed it.
    pub actor: Option<UserId>,
    /// The dotted action name.
    pub action: String,
    /// What was acted on.
    pub target: Option<String>,
    /// Structured detail.
    pub data: serde_json::Value,
    /// When it happened.
    pub created_at: DateTime<Utc>,
}

impl Store {
    // ------------------------------------------------------------ queue bans

    /// Issues a timed queue ban, returning its id.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`](crate::error::ServiceError::Database) if a query fails.
    pub async fn add_queue_ban(
        &self,
        guild: GuildId,
        user: UserId,
        issuer: UserId,
        reason: Option<&str>,
        expires_at: DateTime<Utc>,
    ) -> ServiceResult<i64> {
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO queue_bans (guild_id, user_id, issuer_id, reason, expires_at)
             VALUES ($1, $2, $3, $4, $5) RETURNING id",
        )
        .bind(guild.get())
        .bind(user.get())
        .bind(issuer.get())
        .bind(reason)
        .bind(expires_at)
        .fetch_one(self.pool())
        .await?;
        Ok(id)
    }

    /// When the player's currently active ban ends, if they have one.
    ///
    /// Returns the latest expiry among their unreleased bans, so overlapping
    /// bans behave as one.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`](crate::error::ServiceError::Database) if the query fails.
    pub async fn active_ban_until(
        &self,
        guild: GuildId,
        user: UserId,
        now: DateTime<Utc>,
    ) -> ServiceResult<Option<DateTime<Utc>>> {
        let until: Option<DateTime<Utc>> = sqlx::query_scalar(
            "SELECT max(expires_at) FROM queue_bans
             WHERE guild_id = $1 AND user_id = $2 AND released_at IS NULL AND expires_at > $3",
        )
        .bind(guild.get())
        .bind(user.get())
        .bind(now)
        .fetch_one(self.pool())
        .await?;
        Ok(until)
    }

    /// Every unreleased, unexpired ban in a guild, soonest to expire first.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`](crate::error::ServiceError::Database) if a query fails.
    pub async fn active_bans(
        &self,
        guild: GuildId,
        now: DateTime<Utc>,
    ) -> ServiceResult<Vec<QueueBanRow>> {
        let rows = sqlx::query(
            "SELECT id, guild_id, user_id, issuer_id, reason, started_at, expires_at,
                    released_at, released_by
             FROM queue_bans
             WHERE guild_id = $1 AND released_at IS NULL AND expires_at > $2
             ORDER BY expires_at",
        )
        .bind(guild.get())
        .bind(now)
        .fetch_all(self.pool())
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| QueueBanRow {
                id: row.get("id"),
                guild: GuildId(row.get("guild_id")),
                user: UserId(row.get("user_id")),
                issuer: UserId(row.get("issuer_id")),
                reason: row.get("reason"),
                started_at: row.get("started_at"),
                expires_at: row.get("expires_at"),
                released_at: row.get("released_at"),
                released_by: row.get::<Option<i64>, _>("released_by").map(UserId),
            })
            .collect())
    }

    /// Releases every active ban on a player. Returns how many were lifted.
    ///
    /// Bans are released rather than deleted, so who lifted one and when
    /// remains on the record.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`](crate::error::ServiceError::Database) if the query fails.
    pub async fn release_bans(
        &self,
        guild: GuildId,
        user: UserId,
        released_by: UserId,
    ) -> ServiceResult<u64> {
        let result = sqlx::query(
            "UPDATE queue_bans SET released_at = now(), released_by = $3
             WHERE guild_id = $1 AND user_id = $2 AND released_at IS NULL",
        )
        .bind(guild.get())
        .bind(user.get())
        .bind(released_by.get())
        .execute(self.pool())
        .await?;
        Ok(result.rows_affected())
    }

    // -------------------------------------------------------- player phrases

    /// Adds a join phrase for a player in one channel.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`](crate::error::ServiceError::Database) if a query fails.
    pub async fn add_phrase(
        &self,
        channel: ChannelId,
        user: UserId,
        phrase: &str,
    ) -> ServiceResult<()> {
        sqlx::query("INSERT INTO player_phrases (channel_id, user_id, phrase) VALUES ($1, $2, $3)")
            .bind(channel.get())
            .bind(user.get())
            .bind(phrase)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    /// Removes every join phrase a player has in a channel. Returns how many.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`](crate::error::ServiceError::Database) if a query fails.
    pub async fn clear_phrases(&self, channel: ChannelId, user: UserId) -> ServiceResult<u64> {
        let result =
            sqlx::query("DELETE FROM player_phrases WHERE channel_id = $1 AND user_id = $2")
                .bind(channel.get())
                .bind(user.get())
                .execute(self.pool())
                .await?;
        Ok(result.rows_affected())
    }

    /// One of the player's phrases, chosen at random by the database.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`](crate::error::ServiceError::Database) if the query fails.
    pub async fn random_phrase(
        &self,
        channel: ChannelId,
        user: UserId,
    ) -> ServiceResult<Option<String>> {
        let phrase: Option<String> = sqlx::query_scalar(
            "SELECT phrase FROM player_phrases WHERE channel_id = $1 AND user_id = $2
             ORDER BY random() LIMIT 1",
        )
        .bind(channel.get())
        .bind(user.get())
        .fetch_optional(self.pool())
        .await?;
        Ok(phrase)
    }

    /// A player's join phrases in a channel, oldest first.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`](crate::error::ServiceError::Database) if a query fails.
    pub async fn phrases_for(
        &self,
        channel: ChannelId,
        user: UserId,
    ) -> ServiceResult<Vec<String>> {
        Ok(sqlx::query_scalar(
            "SELECT phrase FROM player_phrases WHERE channel_id = $1 AND user_id = $2
             ORDER BY created_at",
        )
        .bind(channel.get())
        .bind(user.get())
        .fetch_all(self.pool())
        .await?)
    }

    // --------------------------------------------------------- subscriptions

    /// Subscribes a player to a promotion role. Returns whether this was new.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`](crate::error::ServiceError::Database) if a query fails.
    pub async fn subscribe(
        &self,
        channel: ChannelId,
        user: UserId,
        role: RoleId,
    ) -> ServiceResult<bool> {
        let result = sqlx::query(
            "INSERT INTO subscriptions (channel_id, user_id, role_id) VALUES ($1, $2, $3)
             ON CONFLICT DO NOTHING",
        )
        .bind(channel.get())
        .bind(user.get())
        .bind(role.get())
        .execute(self.pool())
        .await?;
        Ok(result.rows_affected() > 0)
    }

    /// Unsubscribes a player. Returns whether they were subscribed.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`](crate::error::ServiceError::Database) if a query fails.
    pub async fn unsubscribe(
        &self,
        channel: ChannelId,
        user: UserId,
        role: RoleId,
    ) -> ServiceResult<bool> {
        let result = sqlx::query(
            "DELETE FROM subscriptions WHERE channel_id = $1 AND user_id = $2 AND role_id = $3",
        )
        .bind(channel.get())
        .bind(user.get())
        .bind(role.get())
        .execute(self.pool())
        .await?;
        Ok(result.rows_affected() > 0)
    }

    /// Everybody subscribed to a promotion role in a channel.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`](crate::error::ServiceError::Database) if a query fails.
    pub async fn subscribers(
        &self,
        channel: ChannelId,
        role: RoleId,
    ) -> ServiceResult<Vec<UserId>> {
        let ids: Vec<i64> = sqlx::query_scalar(
            "SELECT user_id FROM subscriptions WHERE channel_id = $1 AND role_id = $2",
        )
        .bind(channel.get())
        .bind(role.get())
        .fetch_all(self.pool())
        .await?;
        Ok(ids.into_iter().map(UserId).collect())
    }

    // ------------------------------------------------------------ audit log

    /// Records an audited action.
    ///
    /// Every write carries the running mode, so a debug action can never be
    /// mistaken for a production one.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`](crate::error::ServiceError::Database) if the insert
    /// fails.
    pub async fn audit(&self, event: &NewAuditEvent<'_>) -> ServiceResult<()> {
        sqlx::query(
            "INSERT INTO audit_events (guild_id, channel_id, actor_id, action, target, data, mode)
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(event.guild.map(GuildId::get))
        .bind(event.channel.map(ChannelId::get))
        .bind(event.actor.map(UserId::get))
        .bind(event.action)
        .bind(event.target)
        .bind(&event.data)
        .bind(event.mode)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// The most recent audit events for a guild, newest first.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`](crate::error::ServiceError::Database) if a query fails.
    pub async fn recent_audit_events(
        &self,
        guild: GuildId,
        limit: i64,
    ) -> ServiceResult<Vec<AuditEvent>> {
        let rows = sqlx::query(
            "SELECT guild_id, channel_id, actor_id, action, target, data, created_at
             FROM audit_events WHERE guild_id = $1 ORDER BY created_at DESC LIMIT $2",
        )
        .bind(guild.get())
        .bind(limit)
        .fetch_all(self.pool())
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| AuditEvent {
                guild: row.get::<Option<i64>, _>("guild_id").map(GuildId),
                channel: row.get::<Option<i64>, _>("channel_id").map(ChannelId),
                actor: row.get::<Option<i64>, _>("actor_id").map(UserId),
                action: row.get("action"),
                target: row.get("target"),
                data: row.get("data"),
                created_at: row.get("created_at"),
            })
            .collect())
    }
}
