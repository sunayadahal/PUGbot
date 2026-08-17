//! Rating, statistics, and leaderboard persistence.

use chrono::{DateTime, Utc};
use sqlx::postgres::PgRow;
use sqlx::Row;

use super::{Store, Tx};
use crate::domain::ids::{ChannelId, MatchId, UserId};
use crate::domain::rating::{RatingConfig, RatingDelta};
use crate::error::ServiceResult;

/// A row of `channel_players`: one player's standing in one rating pool.
#[derive(Debug, Clone)]
pub struct PlayerStatsRow {
    /// The rating pool this record belongs to.
    pub channel: ChannelId,
    /// The player.
    pub user: UserId,
    /// Their current rating.
    pub rating: f64,
    /// How uncertain the system is about that rating.
    pub deviation: f64,
    /// Glicko-2 volatility. Unused by the other systems.
    pub volatility: f64,
    /// Rated wins.
    pub wins: i32,
    /// Rated losses.
    pub losses: i32,
    /// Rated draws.
    pub draws: i32,
    /// Current run: positive for wins, negative for losses, zero after a draw.
    pub streak: i32,
    /// Whether they are hidden from the leaderboard.
    pub hidden: bool,
    /// When they last played a rated match. Drives inactivity decay.
    pub last_ranked_match_at: Option<DateTime<Utc>>,
}

impl PlayerStatsRow {
    fn from_row(row: &PgRow) -> Self {
        Self {
            channel: ChannelId(row.get("channel_id")),
            user: UserId(row.get("user_id")),
            rating: row.get("rating"),
            deviation: row.get("deviation"),
            volatility: row.get("volatility"),
            wins: row.get("wins"),
            losses: row.get("losses"),
            draws: row.get("draws"),
            streak: row.get("streak"),
            hidden: row.get("hidden"),
            last_ranked_match_at: row.get("last_ranked_match_at"),
        }
    }

    /// The record a player starts with in a pool they have never played in.
    #[must_use]
    pub fn new(channel: ChannelId, user: UserId, config: &RatingConfig) -> Self {
        Self {
            channel,
            user,
            rating: config.initial_rating,
            deviation: config.initial_deviation,
            volatility: config.initial_volatility,
            wins: 0,
            losses: 0,
            draws: 0,
            streak: 0,
            hidden: false,
            last_ranked_match_at: None,
        }
    }

    /// Total rated matches: wins plus losses plus draws.
    #[must_use]
    pub fn matches_played(&self) -> i32 {
        self.wins + self.losses + self.draws
    }

    /// Wins as a percentage of matches played, or zero for a player with none.
    #[must_use]
    pub fn win_rate(&self) -> f64 {
        let played = self.matches_played();
        if played == 0 {
            0.0
        } else {
            f64::from(self.wins) / f64::from(played) * 100.0
        }
    }
}

/// How a rating changed and why, for the audit trail.
#[derive(Debug, Clone)]
pub struct RatingHistoryRow {
    /// The player whose rating changed.
    pub user: UserId,
    /// The match responsible, or `None` for a manual adjustment or decay.
    pub match_id: Option<MatchId>,
    /// Rating before the change.
    pub rating_before: f64,
    /// Rating after the change.
    pub rating_after: f64,
    /// Deviation before the change.
    pub deviation_before: f64,
    /// Deviation after the change.
    pub deviation_after: f64,
    /// Human-readable cause, such as `match #42` or a moderator's reason.
    pub reason: String,
    /// The moderator or administrator responsible, for manual adjustments.
    pub actor: Option<UserId>,
    /// When the change was recorded.
    pub created_at: DateTime<Utc>,
}

const STATS_COLUMNS: &str = "channel_id, user_id, rating, deviation, volatility, wins, losses, \
     draws, streak, hidden, last_ranked_match_at";

impl Store {
    /// One player's record in a rating pool, or `None` if they have never
    /// played there.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`](crate::error::ServiceError::Database) if a query fails.
    pub async fn player_stats(
        &self,
        channel: ChannelId,
        user: UserId,
    ) -> ServiceResult<Option<PlayerStatsRow>> {
        let row = sqlx::query(&format!(
            "SELECT {STATS_COLUMNS} FROM channel_players WHERE channel_id = $1 AND user_id = $2"
        ))
        .bind(channel.get())
        .bind(user.get())
        .fetch_optional(self.pool())
        .await?;
        Ok(row.as_ref().map(PlayerStatsRow::from_row))
    }

    /// Loads stats for a whole roster in one round trip, defaulting anybody who
    /// has never played here to the configured starting values.
    ///
    /// Results are returned in the same order as `users`.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`](crate::error::ServiceError::Database) if the query fails.
    pub async fn player_stats_bulk(
        &self,
        channel: ChannelId,
        users: &[UserId],
        config: &RatingConfig,
    ) -> ServiceResult<Vec<PlayerStatsRow>> {
        if users.is_empty() {
            return Ok(Vec::new());
        }
        let ids: Vec<i64> = users.iter().map(|u| u.get()).collect();
        let existing = sqlx::query(&format!(
            "SELECT {STATS_COLUMNS} FROM channel_players
             WHERE channel_id = $1 AND user_id = ANY($2)"
        ))
        .bind(channel.get())
        .bind(&ids)
        .fetch_all(self.pool())
        .await?;
        let existing: Vec<PlayerStatsRow> = existing.iter().map(PlayerStatsRow::from_row).collect();

        Ok(users
            .iter()
            .map(|user| {
                existing
                    .iter()
                    .find(|row| row.user == *user)
                    .cloned()
                    .unwrap_or_else(|| PlayerStatsRow::new(channel, *user, config))
            })
            .collect())
    }

    /// Writes a player's record inside the caller's transaction.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`](crate::error::ServiceError::Database) if a query fails.
    pub async fn upsert_player_stats(
        &self,
        tx: &mut Tx<'_>,
        stats: &PlayerStatsRow,
    ) -> ServiceResult<()> {
        sqlx::query(
            "INSERT INTO channel_players (channel_id, user_id, rating, deviation, volatility,
                                          wins, losses, draws, streak, hidden,
                                          last_ranked_match_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, now())
             ON CONFLICT (channel_id, user_id) DO UPDATE SET
                 rating = $3, deviation = $4, volatility = $5, wins = $6, losses = $7,
                 draws = $8, streak = $9, hidden = $10, last_ranked_match_at = $11,
                 updated_at = now()",
        )
        .bind(stats.channel.get())
        .bind(stats.user.get())
        .bind(stats.rating)
        .bind(stats.deviation)
        .bind(stats.volatility)
        .bind(stats.wins)
        .bind(stats.losses)
        .bind(stats.draws)
        .bind(stats.streak)
        .bind(stats.hidden)
        .bind(stats.last_ranked_match_at)
        .execute(&mut **tx)
        .await?;
        Ok(())
    }

    /// Writes one rating-history row, returning whether it was inserted.
    ///
    /// For match-derived changes the unique index on `(match_id, user_id)`
    /// makes a retry a silent no-op, so a match can never be rated twice.
    /// Manual adjustments carry no match id and are therefore never
    /// deduplicated — two identical penalties are two real events.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`](crate::error::ServiceError::Database) if the query fails.
    pub async fn record_rating_change(
        &self,
        tx: &mut Tx<'_>,
        channel: ChannelId,
        match_id: Option<MatchId>,
        delta: &RatingDelta,
        reason: &str,
        actor: Option<UserId>,
    ) -> ServiceResult<bool> {
        let result = sqlx::query(
            "INSERT INTO rating_history (channel_id, user_id, match_id, rating_before,
                                         rating_after, deviation_before, deviation_after,
                                         reason, actor_id)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
             ON CONFLICT (match_id, user_id) WHERE match_id IS NOT NULL DO NOTHING",
        )
        .bind(channel.get())
        .bind(delta.user.get())
        .bind(match_id.map(MatchId::get))
        .bind(delta.rating_before)
        .bind(delta.rating_after)
        .bind(delta.deviation_before)
        .bind(delta.deviation_after)
        .bind(reason)
        .bind(actor.map(UserId::get))
        .execute(&mut **tx)
        .await?;
        Ok(result.rows_affected() > 0)
    }

    /// A player's most recent rating changes, newest first.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`](crate::error::ServiceError::Database) if a query fails.
    pub async fn rating_history(
        &self,
        channel: ChannelId,
        user: UserId,
        limit: i64,
    ) -> ServiceResult<Vec<RatingHistoryRow>> {
        let rows = sqlx::query(
            "SELECT user_id, match_id, rating_before, rating_after, deviation_before,
                    deviation_after, reason, actor_id, created_at
             FROM rating_history
             WHERE channel_id = $1 AND user_id = $2
             ORDER BY created_at DESC LIMIT $3",
        )
        .bind(channel.get())
        .bind(user.get())
        .bind(limit)
        .fetch_all(self.pool())
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| RatingHistoryRow {
                user: UserId(row.get("user_id")),
                match_id: row.get::<Option<i64>, _>("match_id").map(MatchId),
                rating_before: row.get("rating_before"),
                rating_after: row.get("rating_after"),
                deviation_before: row.get("deviation_before"),
                deviation_after: row.get("deviation_after"),
                reason: row.get("reason"),
                actor: row.get::<Option<i64>, _>("actor_id").map(UserId),
                created_at: row.get("created_at"),
            })
            .collect())
    }

    /// One page of the leaderboard, plus the total number of qualifying
    /// players.
    ///
    /// Honours the minimum-match and recent-activity cutoffs, and excludes
    /// hidden players.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`](crate::error::ServiceError::Database) if a query fails.
    pub async fn leaderboard(
        &self,
        channel: ChannelId,
        min_matches: i32,
        activity_days: i32,
        offset: i64,
        limit: i64,
    ) -> ServiceResult<(Vec<PlayerStatsRow>, i64)> {
        let filter = "channel_id = $1 AND NOT hidden
              AND (wins + losses + draws) >= $2
              AND ($3 <= 0 OR last_ranked_match_at > now() - make_interval(days => $3))";
        let rows = sqlx::query(&format!(
            "SELECT {STATS_COLUMNS} FROM channel_players WHERE {filter}
             ORDER BY rating DESC, (wins + losses + draws) DESC, user_id
             OFFSET $4 LIMIT $5"
        ))
        .bind(channel.get())
        .bind(min_matches)
        .bind(activity_days)
        .bind(offset)
        .bind(limit)
        .fetch_all(self.pool())
        .await?;
        let total: i64 = sqlx::query_scalar(&format!(
            "SELECT count(*) FROM channel_players WHERE {filter}"
        ))
        .bind(channel.get())
        .bind(min_matches)
        .bind(activity_days)
        .fetch_one(self.pool())
        .await?;
        Ok((rows.iter().map(PlayerStatsRow::from_row).collect(), total))
    }

    /// A player's position on the leaderboard, 1-based.
    /// A player's one-based position on the leaderboard, ignoring the minimum
    /// match and activity cutoffs.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`](crate::error::ServiceError::Database) if a query fails.
    pub async fn leaderboard_position(
        &self,
        channel: ChannelId,
        user: UserId,
    ) -> ServiceResult<Option<i64>> {
        let position: Option<i64> = sqlx::query_scalar(
            "SELECT position FROM (
                 SELECT user_id, row_number() OVER (ORDER BY rating DESC, user_id) AS position
                 FROM channel_players WHERE channel_id = $1 AND NOT hidden
             ) ranked WHERE user_id = $2",
        )
        .bind(channel.get())
        .bind(user.get())
        .fetch_optional(self.pool())
        .await?;
        Ok(position)
    }

    /// Most active players, for `/top`.
    /// The players with the most matches, for `/top`.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`](crate::error::ServiceError::Database) if a query fails.
    pub async fn most_active(
        &self,
        channel: ChannelId,
        limit: i64,
    ) -> ServiceResult<Vec<PlayerStatsRow>> {
        let rows = sqlx::query(&format!(
            "SELECT {STATS_COLUMNS} FROM channel_players
             WHERE channel_id = $1 AND NOT hidden
             ORDER BY (wins + losses + draws) DESC, rating DESC, user_id LIMIT $2"
        ))
        .bind(channel.get())
        .bind(limit)
        .fetch_all(self.pool())
        .await?;
        Ok(rows.iter().map(PlayerStatsRow::from_row).collect())
    }

    /// Players who have not played for a while, for the decay job.
    /// Players whose last rated match predates `since`. The decay job's work
    /// list.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`](crate::error::ServiceError::Database) if a query fails.
    pub async fn inactive_players(
        &self,
        channel: ChannelId,
        since: DateTime<Utc>,
    ) -> ServiceResult<Vec<PlayerStatsRow>> {
        let rows = sqlx::query(&format!(
            "SELECT {STATS_COLUMNS} FROM channel_players
             WHERE channel_id = $1 AND last_ranked_match_at IS NOT NULL
               AND last_ranked_match_at < $2"
        ))
        .bind(channel.get())
        .bind(since)
        .fetch_all(self.pool())
        .await?;
        Ok(rows.iter().map(PlayerStatsRow::from_row).collect())
    }

    /// Hides or unhides a player on the leaderboard. Returns whether they had a
    /// record to change.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`](crate::error::ServiceError::Database) if a query fails.
    pub async fn set_player_hidden(
        &self,
        channel: ChannelId,
        user: UserId,
        hidden: bool,
    ) -> ServiceResult<bool> {
        let result = sqlx::query(
            "UPDATE channel_players SET hidden = $3, updated_at = now()
             WHERE channel_id = $1 AND user_id = $2",
        )
        .bind(channel.get())
        .bind(user.get())
        .bind(hidden)
        .execute(self.pool())
        .await?;
        Ok(result.rows_affected() > 0)
    }

    /// Wipes a channel's ratings and their history.
    /// Deletes every rating and history row in a pool. Returns how many player
    /// records were removed.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`](crate::error::ServiceError::Database) if a query fails.
    pub async fn reset_channel_stats(&self, channel: ChannelId) -> ServiceResult<u64> {
        let mut tx = self.begin().await?;
        sqlx::query("DELETE FROM rating_history WHERE channel_id = $1")
            .bind(channel.get())
            .execute(&mut *tx)
            .await?;
        let result = sqlx::query("DELETE FROM channel_players WHERE channel_id = $1")
            .bind(channel.get())
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(result.rows_affected())
    }

    /// Deletes one player's rating and history. Returns whether they had any.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`](crate::error::ServiceError::Database) if a query fails.
    pub async fn reset_player_stats(
        &self,
        channel: ChannelId,
        user: UserId,
    ) -> ServiceResult<bool> {
        let mut tx = self.begin().await?;
        sqlx::query("DELETE FROM rating_history WHERE channel_id = $1 AND user_id = $2")
            .bind(channel.get())
            .bind(user.get())
            .execute(&mut *tx)
            .await?;
        let result =
            sqlx::query("DELETE FROM channel_players WHERE channel_id = $1 AND user_id = $2")
                .bind(channel.get())
                .bind(user.get())
                .execute(&mut *tx)
                .await?;
        tx.commit().await?;
        Ok(result.rows_affected() > 0)
    }

    /// Moves one player's record onto another user id, merging if the target
    /// already exists. Used when somebody changes Discord account.
    ///
    /// Rating history follows the record, so the new account inherits the old
    /// one's audit trail rather than starting blank.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`](crate::error::ServiceError::Database) if a query fails.
    pub async fn replace_player(
        &self,
        channel: ChannelId,
        from: UserId,
        into: UserId,
    ) -> ServiceResult<bool> {
        let mut tx = self.begin().await?;
        let moved = sqlx::query(
            "UPDATE channel_players SET user_id = $3 WHERE channel_id = $1 AND user_id = $2
             AND NOT EXISTS (SELECT 1 FROM channel_players
                             WHERE channel_id = $1 AND user_id = $3)",
        )
        .bind(channel.get())
        .bind(from.get())
        .bind(into.get())
        .execute(&mut *tx)
        .await?;

        if moved.rows_affected() == 0 {
            // The target already has a record: fold the source into it and
            // drop the source row.
            sqlx::query(
                "UPDATE channel_players AS target SET
                     wins = target.wins + source.wins,
                     losses = target.losses + source.losses,
                     draws = target.draws + source.draws,
                     updated_at = now()
                 FROM channel_players AS source
                 WHERE target.channel_id = $1 AND target.user_id = $3
                   AND source.channel_id = $1 AND source.user_id = $2",
            )
            .bind(channel.get())
            .bind(from.get())
            .bind(into.get())
            .execute(&mut *tx)
            .await?;
            sqlx::query("DELETE FROM channel_players WHERE channel_id = $1 AND user_id = $2")
                .bind(channel.get())
                .bind(from.get())
                .execute(&mut *tx)
                .await?;
        }

        sqlx::query(
            "UPDATE rating_history SET user_id = $3 WHERE channel_id = $1 AND user_id = $2",
        )
        .bind(channel.get())
        .bind(from.get())
        .bind(into.get())
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(true)
    }
}
