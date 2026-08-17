//! Persistence.
//!
//! `Store` owns the connection pool and exposes one method per use-case query.
//! Queries are written with the runtime `sqlx::query` API rather than the
//! compile-time macros so that `cargo build` and `cargo clippy` work without a
//! live database; correctness of the SQL is covered by the integration tests in
//! `tests/`, which run against a real PostgreSQL instance.
//!
//! Multi-statement writes take an explicit transaction, and no transaction is
//! ever held across a Discord network call.

pub mod matches;
pub mod moderation;
pub mod ratings;

use chrono::{DateTime, Utc};
use sqlx::postgres::{PgPoolOptions, PgRow};
use sqlx::{PgPool, Postgres, Row, Transaction};

use crate::config::AppConfig;
use crate::domain::ids::{ChannelId, GuildId, QueueId, UserId};
use crate::domain::settings::{ChannelSettings, QueueSettings};
use crate::error::{ServiceError, ServiceResult};

/// A database transaction.
pub type Tx<'a> = Transaction<'a, Postgres>;

/// The database gateway.
///
/// Holds the connection pool and exposes one method per use-case query. Cheap
/// to clone: the pool is shared.
#[derive(Debug, Clone)]
pub struct Store {
    pool: PgPool,
}

impl Store {
    /// Opens a connection pool sized by the configuration.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`] if the database is unreachable or
    /// rejects the credentials.
    pub async fn connect(config: &AppConfig) -> ServiceResult<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(config.database_max_connections)
            .connect(config.database_url.expose())
            .await?;
        Ok(Self { pool })
    }

    /// Wraps an existing pool. Used by tests, which pin each connection to a
    /// throwaway schema.
    pub fn from_pool(pool: PgPool) -> Self {
        Self { pool }
    }

    /// The underlying pool, for the few call sites that issue a one-off query.
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Starts a transaction.
    ///
    /// Every multi-statement write takes one. No transaction is ever held
    /// across a Discord network call.
    //////
    /// # Errors
    /// Returns [`ServiceError::Database`] if the query fails.
    pub async fn begin(&self) -> ServiceResult<Tx<'static>> {
        Ok(self.pool.begin().await?)
    }

    /// Applies every migration in `migrations/`.
    ///
    /// Idempotent: re-running applies nothing, which is what makes a deploy
    /// that restarts several replicas safe.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Other`] if a migration fails or the recorded
    /// history diverges from the files on disk.
    pub async fn migrate(&self) -> ServiceResult<()> {
        sqlx::migrate!("./migrations")
            .run(&self.pool)
            .await
            .map_err(|e| ServiceError::Other(e.into()))
    }

    /// Cheap liveness probe for the health endpoint.
    /// Cheap liveness probe for the readiness endpoint.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`] if the query fails.
    pub async fn ping(&self) -> ServiceResult<()> {
        sqlx::query("SELECT 1").execute(&self.pool).await?;
        Ok(())
    }

    // ---------------------------------------------------------------- guilds

    /// Creates or updates a guild row.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`] if the query fails.
    pub async fn upsert_guild(&self, guild: GuildId, enabled: bool) -> ServiceResult<()> {
        sqlx::query(
            "INSERT INTO guilds (guild_id, enabled) VALUES ($1, $2)
             ON CONFLICT (guild_id) DO UPDATE SET enabled = $2, updated_at = now()",
        )
        .bind(guild.get())
        .bind(enabled)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Whether the guild is enabled. Unknown guilds count as enabled, so a
    /// guild works the moment the bot joins it.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`] if the query fails.
    pub async fn guild_enabled(&self, guild: GuildId) -> ServiceResult<bool> {
        let enabled: Option<bool> =
            sqlx::query_scalar("SELECT enabled FROM guilds WHERE guild_id = $1")
                .bind(guild.get())
                .fetch_optional(&self.pool)
                .await?;
        Ok(enabled.unwrap_or(true))
    }

    // -------------------------------------------------------------- channels

    /// Enables a channel, creating the guild row if this is its first channel.
    ///
    /// Both writes happen in one transaction, so a crash cannot leave a channel
    /// referencing a guild that does not exist.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`] if the query fails.
    pub async fn enable_channel(
        &self,
        guild: GuildId,
        channel: ChannelId,
        settings: &ChannelSettings,
    ) -> ServiceResult<()> {
        let mut tx = self.begin().await?;
        sqlx::query("INSERT INTO guilds (guild_id) VALUES ($1) ON CONFLICT (guild_id) DO NOTHING")
            .bind(guild.get())
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "INSERT INTO channel_configs (channel_id, guild_id, enabled, settings)
             VALUES ($1, $2, TRUE, $3)
             ON CONFLICT (channel_id)
             DO UPDATE SET enabled = TRUE, guild_id = $2, updated_at = now()",
        )
        .bind(channel.get())
        .bind(guild.get())
        .bind(serde_json::to_value(settings).map_err(|e| ServiceError::Other(e.into()))?)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Disables a channel without deleting its queue, ratings, or history.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`] if the query fails.
    pub async fn disable_channel(&self, channel: ChannelId) -> ServiceResult<()> {
        sqlx::query(
            "UPDATE channel_configs SET enabled = FALSE, updated_at = now() WHERE channel_id = $1",
        )
        .bind(channel.get())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Returns the channel's configuration, or `None` if it was never enabled.
    /// The channel's configuration, or `None` if it was never enabled.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`] if the query fails.
    pub async fn channel_config(
        &self,
        channel: ChannelId,
    ) -> ServiceResult<Option<ChannelConfigRow>> {
        let row = sqlx::query(
            "SELECT channel_id, guild_id, enabled, settings FROM channel_configs WHERE channel_id = $1",
        )
        .bind(channel.get())
        .fetch_optional(&self.pool)
        .await?;
        row.map(ChannelConfigRow::from_row).transpose()
    }

    /// The configuration of an enabled channel, or a typed rejection.
    /// The configuration of an enabled channel.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::ChannelNotEnabled`] if the channel is unknown or
    /// disabled, or [`ServiceError::Database`] if the query fails.
    pub async fn require_enabled_channel(
        &self,
        channel: ChannelId,
    ) -> ServiceResult<ChannelConfigRow> {
        match self.channel_config(channel).await? {
            Some(config) if config.enabled => Ok(config),
            _ => Err(ServiceError::ChannelNotEnabled),
        }
    }

    /// Overwrites a channel's settings blob.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`] if the query fails.
    pub async fn update_channel_settings(
        &self,
        channel: ChannelId,
        settings: &ChannelSettings,
    ) -> ServiceResult<()> {
        sqlx::query(
            "UPDATE channel_configs SET settings = $2, updated_at = now() WHERE channel_id = $1",
        )
        .bind(channel.get())
        .bind(serde_json::to_value(settings).map_err(|e| ServiceError::Other(e.into()))?)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Every enabled channel, for the decay job and the intent calculation.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`] if the query fails.
    pub async fn enabled_channels(&self) -> ServiceResult<Vec<ChannelConfigRow>> {
        let rows = sqlx::query(
            "SELECT channel_id, guild_id, enabled, settings FROM channel_configs WHERE enabled",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter().map(ChannelConfigRow::from_row).collect()
    }

    // ---------------------------------------------------------------- queues

    /// Creates the channel's single queue. The unique constraint on
    /// `channel_id` turns a race between two administrators into a clean
    /// `QueueExists` rejection rather than two queues.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::QueueExists`] if the channel already has a
    /// queue, or [`ServiceError::Database`] if the insert fails.
    pub async fn create_queue(
        &self,
        guild: GuildId,
        channel: ChannelId,
        settings: &QueueSettings,
    ) -> ServiceResult<QueueId> {
        let result = sqlx::query_scalar::<_, i64>(
            "INSERT INTO queues (channel_id, guild_id, settings) VALUES ($1, $2, $3)
             RETURNING queue_id",
        )
        .bind(channel.get())
        .bind(guild.get())
        .bind(serde_json::to_value(settings).map_err(|e| ServiceError::Other(e.into()))?)
        .fetch_one(&self.pool)
        .await;

        match result {
            Ok(id) => Ok(QueueId(id)),
            Err(sqlx::Error::Database(db)) if db.is_unique_violation() => {
                Err(ServiceError::QueueExists)
            }
            Err(other) => Err(other.into()),
        }
    }

    /// The channel's queue, or `None` if none has been created.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`] if the query fails.
    pub async fn queue_for_channel(&self, channel: ChannelId) -> ServiceResult<Option<QueueRow>> {
        let row = sqlx::query(
            "SELECT queue_id, channel_id, guild_id, settings, last_promoted_at
             FROM queues WHERE channel_id = $1",
        )
        .bind(channel.get())
        .fetch_optional(&self.pool)
        .await?;
        row.map(QueueRow::from_row).transpose()
    }

    /// The channel's queue.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::NoQueue`] if the channel has none.
    pub async fn require_queue(&self, channel: ChannelId) -> ServiceResult<QueueRow> {
        self.queue_for_channel(channel)
            .await?
            .ok_or(ServiceError::NoQueue)
    }

    /// Overwrites a queue's settings blob.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`] if the query fails.
    pub async fn update_queue_settings(
        &self,
        queue: QueueId,
        settings: &QueueSettings,
    ) -> ServiceResult<()> {
        sqlx::query("UPDATE queues SET settings = $2, updated_at = now() WHERE queue_id = $1")
            .bind(queue.get())
            .bind(serde_json::to_value(settings).map_err(|e| ServiceError::Other(e.into()))?)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Deletes a queue and, by cascade, its membership rows. Matches survive,
    /// with their queue reference set to null.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`] if the query fails.
    pub async fn delete_queue(&self, queue: QueueId) -> ServiceResult<()> {
        sqlx::query("DELETE FROM queues WHERE queue_id = $1")
            .bind(queue.get())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Records a promotion, starting the cooldown.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`] if the query fails.
    pub async fn mark_promoted(&self, queue: QueueId, at: DateTime<Utc>) -> ServiceResult<()> {
        sqlx::query("UPDATE queues SET last_promoted_at = $2 WHERE queue_id = $1")
            .bind(queue.get())
            .bind(at)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // -------------------------------------------------------- queue members

    /// Adds a player to the queue. Returns `false` if they were already in it,
    /// which makes concurrent `/add` presses idempotent rather than an error.
    /// Adds a player without checking capacity.
    ///
    /// Returns `false` if they already held a slot, which makes a duplicated
    /// call idempotent rather than an error. Prefer
    /// [`Store::add_queue_member_atomic`] for player-initiated joins, which
    /// also enforces the queue size.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`] if the query fails.
    pub async fn add_queue_member(
        &self,
        queue: QueueId,
        user: UserId,
        joined_at: DateTime<Utc>,
        expires_at: Option<DateTime<Utc>>,
    ) -> ServiceResult<bool> {
        let result = sqlx::query(
            "INSERT INTO queue_members (queue_id, user_id, joined_at, expires_at)
             VALUES ($1, $2, $3, $4) ON CONFLICT (queue_id, user_id) DO NOTHING",
        )
        .bind(queue.get())
        .bind(user.get())
        .bind(joined_at)
        .bind(expires_at)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    /// Adds a player only if the queue still has room.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::NoQueue`] if the queue has been deleted, or
    /// [`ServiceError::Database`] if a query fails.
    ///
    /// The capacity check and the insert must be one atomic step: checking the
    /// size in the service and then inserting lets simultaneous `/add` presses
    /// both pass the check and overfill the queue. A row lock on the queue
    /// serialises joins to that one queue, which is the narrowest scope that
    /// makes the count trustworthy.
    pub async fn add_queue_member_atomic(
        &self,
        queue: QueueId,
        size: u32,
        user: UserId,
        joined_at: DateTime<Utc>,
        expires_at: Option<DateTime<Utc>>,
    ) -> ServiceResult<QueueInsert> {
        let mut tx = self.begin().await?;

        let locked: Option<i64> =
            sqlx::query_scalar("SELECT queue_id FROM queues WHERE queue_id = $1 FOR UPDATE")
                .bind(queue.get())
                .fetch_optional(&mut *tx)
                .await?;
        if locked.is_none() {
            return Err(ServiceError::NoQueue);
        }

        let already: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM queue_members WHERE queue_id = $1 AND user_id = $2)",
        )
        .bind(queue.get())
        .bind(user.get())
        .fetch_one(&mut *tx)
        .await?;
        if already {
            return Ok(QueueInsert::Duplicate);
        }

        let count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM queue_members WHERE queue_id = $1")
                .bind(queue.get())
                .fetch_one(&mut *tx)
                .await?;
        if count >= i64::from(size) {
            return Ok(QueueInsert::Full);
        }

        sqlx::query(
            "INSERT INTO queue_members (queue_id, user_id, joined_at, expires_at)
             VALUES ($1, $2, $3, $4)",
        )
        .bind(queue.get())
        .bind(user.get())
        .bind(joined_at)
        .bind(expires_at)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;

        Ok(QueueInsert::Added {
            total: (count + 1) as usize,
        })
    }

    /// Removes a player's slot. Returns whether they had one.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`] if the query fails.
    pub async fn remove_queue_member(&self, queue: QueueId, user: UserId) -> ServiceResult<bool> {
        let result = sqlx::query("DELETE FROM queue_members WHERE queue_id = $1 AND user_id = $2")
            .bind(queue.get())
            .bind(user.get())
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected() > 0)
    }

    /// The queue's members in join order.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`] if the query fails.
    pub async fn queue_members(&self, queue: QueueId) -> ServiceResult<Vec<QueueMemberRow>> {
        let rows = sqlx::query(
            "SELECT user_id, joined_at, expires_at FROM queue_members
             WHERE queue_id = $1 ORDER BY joined_at, user_id",
        )
        .bind(queue.get())
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(QueueMemberRow::from_row).collect())
    }

    /// Empties a queue. Returns how many slots were removed.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`] if the query fails.
    pub async fn clear_queue(&self, queue: QueueId) -> ServiceResult<u64> {
        let result = sqlx::query("DELETE FROM queue_members WHERE queue_id = $1")
            .bind(queue.get())
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }

    /// Removes every expired member across all queues and reports who was
    /// dropped, so the sweeper can tell them why.
    ///
    /// Idempotent by construction: the delete is the only state change and it
    /// only ever matches rows already past due, so a second run finds nothing.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`] if the query fails.
    pub async fn remove_expired_members(
        &self,
        now: DateTime<Utc>,
    ) -> ServiceResult<Vec<(QueueId, UserId)>> {
        let rows = sqlx::query(
            "DELETE FROM queue_members
             WHERE expires_at IS NOT NULL AND expires_at <= $1
             RETURNING queue_id, user_id",
        )
        .bind(now)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| {
                (
                    QueueId(row.get::<i64, _>("queue_id")),
                    UserId(row.get::<i64, _>("user_id")),
                )
            })
            .collect())
    }

    /// Every queue the player currently sits in, for presence sweeps.
    /// Every queue the player currently sits in, across all channels. Used by
    /// the presence sweep and by queue bans.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`] if the query fails.
    pub async fn queues_for_member(&self, user: UserId) -> ServiceResult<Vec<QueueId>> {
        let ids: Vec<i64> =
            sqlx::query_scalar("SELECT queue_id FROM queue_members WHERE user_id = $1")
                .bind(user.get())
                .fetch_all(&self.pool)
                .await?;
        Ok(ids.into_iter().map(QueueId).collect())
    }

    // ----------------------------------------------------------------- users

    /// The player's personal preferences, defaulted if they have never set any.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`] if the query fails.
    pub async fn user_prefs(&self, user: UserId) -> ServiceResult<UserPrefsRow> {
        let row = sqlx::query(
            "SELECT user_id, dm_on_start, default_expiry_seconds, allow_offline_until,
                    auto_ready_until
             FROM users WHERE user_id = $1",
        )
        .bind(user.get())
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map_or_else(|| UserPrefsRow::default_for(user), UserPrefsRow::from_row))
    }

    /// Writes the player's personal preferences.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`] if the query fails.
    pub async fn save_user_prefs(&self, prefs: &UserPrefsRow) -> ServiceResult<()> {
        sqlx::query(
            "INSERT INTO users (user_id, dm_on_start, default_expiry_seconds,
                                allow_offline_until, auto_ready_until, updated_at)
             VALUES ($1, $2, $3, $4, $5, now())
             ON CONFLICT (user_id) DO UPDATE SET
                 dm_on_start = $2,
                 default_expiry_seconds = $3,
                 allow_offline_until = $4,
                 auto_ready_until = $5,
                 updated_at = now()",
        )
        .bind(prefs.user.get())
        .bind(prefs.dm_on_start)
        .bind(prefs.default_expiry_seconds)
        .bind(prefs.allow_offline_until)
        .bind(prefs.auto_ready_until)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Clears a one-use auto-ready arming after it has been consumed.
    /// Clears a one-use auto-ready arming after it has been applied.
    ///
    /// A no-op for an empty slice.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`] if the query fails.
    pub async fn consume_auto_ready(&self, users: &[UserId]) -> ServiceResult<()> {
        if users.is_empty() {
            return Ok(());
        }
        let ids: Vec<i64> = users.iter().map(|u| u.get()).collect();
        sqlx::query("UPDATE users SET auto_ready_until = NULL WHERE user_id = ANY($1)")
            .bind(&ids)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    // ------------------------------------------------------------ job locks

    /// Takes or renews a lease on a named background job.
    ///
    /// Returns `false` when another holder's lease is still valid, which is how
    /// two processes in the same mode avoid running the same sweep twice. The
    /// current holder can always renew, so a slow sweep does not lose its lease
    /// mid-run. The lease key includes the mode, so debug never contends with
    /// production.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`] if the query fails.
    pub async fn try_acquire_job_lock(
        &self,
        name: &str,
        mode: &str,
        holder: &str,
        until: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> ServiceResult<bool> {
        let result = sqlx::query(
            "INSERT INTO job_locks (name, mode, holder, locked_until)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (name, mode) DO UPDATE
               SET holder = $3, locked_until = $4
               WHERE job_locks.locked_until <= $5 OR job_locks.holder = $3",
        )
        .bind(name)
        .bind(mode)
        .bind(holder)
        .bind(until)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() > 0)
    }
}

// ---------------------------------------------------------------- row types

/// Outcome of an atomic queue join.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueInsert {
    /// The player was added.
    Added {
        /// How many players are queued now, including this one.
        total: usize,
    },
    /// The queue was already at its configured size.
    Full,
    /// The player already held a slot.
    Duplicate,
}

/// A row of `channel_configs`.
#[derive(Debug, Clone)]
pub struct ChannelConfigRow {
    /// The channel this configuration belongs to.
    pub channel: ChannelId,
    /// The guild the channel is in.
    pub guild: GuildId,
    /// Whether PUGbot currently serves this channel.
    pub enabled: bool,
    /// The deserialised settings blob.
    pub settings: ChannelSettings,
}

impl ChannelConfigRow {
    fn from_row(row: PgRow) -> ServiceResult<Self> {
        Ok(Self {
            channel: ChannelId(row.get("channel_id")),
            guild: GuildId(row.get("guild_id")),
            enabled: row.get("enabled"),
            settings: serde_json::from_value(row.get("settings"))
                .map_err(|e| ServiceError::Other(e.into()))?,
        })
    }

    /// The channel whose rating rows this channel reads and writes.
    #[must_use]
    pub fn rating_pool(&self) -> ChannelId {
        self.settings.rating_pool(self.channel)
    }
}

/// A row of `queues`: the single queue belonging to one channel.
#[derive(Debug, Clone)]
pub struct QueueRow {
    /// Primary key.
    pub id: QueueId,
    /// The channel that owns this queue. Unique across the table.
    pub channel: ChannelId,
    /// The guild the channel is in.
    pub guild: GuildId,
    /// The deserialised settings blob.
    pub settings: QueueSettings,
    /// When `/promote` last ran here, for the cooldown.
    pub last_promoted_at: Option<DateTime<Utc>>,
}

impl QueueRow {
    fn from_row(row: PgRow) -> ServiceResult<Self> {
        Ok(Self {
            id: QueueId(row.get("queue_id")),
            channel: ChannelId(row.get("channel_id")),
            guild: GuildId(row.get("guild_id")),
            settings: serde_json::from_value(row.get("settings"))
                .map_err(|e| ServiceError::Other(e.into()))?,
            last_promoted_at: row.get("last_promoted_at"),
        })
    }
}

/// A row of `queue_members`.
#[derive(Debug, Clone)]
pub struct QueueMemberRow {
    /// The queued player.
    pub user: UserId,
    /// When they joined.
    pub joined_at: DateTime<Utc>,
    /// When their slot lapses, if ever.
    pub expires_at: Option<DateTime<Utc>>,
}

impl QueueMemberRow {
    fn from_row(row: PgRow) -> Self {
        Self {
            user: UserId(row.get("user_id")),
            joined_at: row.get("joined_at"),
            expires_at: row.get("expires_at"),
        }
    }
}

/// A row of `users`: one player's cross-channel preferences.
#[derive(Debug, Clone)]
pub struct UserPrefsRow {
    /// The player these preferences belong to.
    pub user: UserId,
    /// Whether to send them a direct message when their match starts.
    pub dm_on_start: bool,
    /// Their preferred queue expiry, overriding the channel default.
    pub default_expiry_seconds: Option<i64>,
    /// Until when they may stay queued while offline.
    pub allow_offline_until: Option<DateTime<Utc>>,
    /// Until when they are automatically marked ready. One use only.
    pub auto_ready_until: Option<DateTime<Utc>>,
}

impl UserPrefsRow {
    /// The defaults applied to a player who has never changed a setting.
    #[must_use]
    pub fn default_for(user: UserId) -> Self {
        Self {
            user,
            dm_on_start: true,
            default_expiry_seconds: None,
            allow_offline_until: None,
            auto_ready_until: None,
        }
    }

    fn from_row(row: PgRow) -> Self {
        Self {
            user: UserId(row.get("user_id")),
            dm_on_start: row.get("dm_on_start"),
            default_expiry_seconds: row.get("default_expiry_seconds"),
            allow_offline_until: row.get("allow_offline_until"),
            auto_ready_until: row.get("auto_ready_until"),
        }
    }
}
