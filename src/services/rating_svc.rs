//! Ratings, ranks, leaderboards, decay, and the rating administration tools.

use chrono::Duration;

use super::AppContext;
use crate::domain::ids::{ChannelId, UserId};
use crate::domain::rating::{
    apply_decay, MatchOutcome, PlayerRatingState, RatingDelta, RatingSystemKind,
};
use crate::domain::report::ReportOutcome;
use crate::domain::settings::RankTier;
use crate::error::{ServiceError, ServiceResult};
use crate::repositories::matches::LoadedMatch;
use crate::repositories::ratings::PlayerStatsRow;
use crate::repositories::{ChannelConfigRow, Tx};

/// Ratings, ranks, leaderboards, decay, and the administration tools.
#[derive(Debug, Clone)]
pub struct RatingService {
    ctx: AppContext,
}

/// A player's standing, as `/rank` renders it.
#[derive(Debug, Clone)]
pub struct RankView {
    /// The player's stored record.
    pub stats: PlayerStatsRow,
    /// One-based leaderboard position, if they appear on it.
    pub position: Option<i64>,
    /// The tier their rating earns, if any.
    pub rank: Option<RankTier>,
    /// How much their rating moved most recently.
    pub last_change: Option<f64>,
}

impl RatingService {
    /// Wraps the shared application context.
    #[must_use]
    pub fn new(ctx: AppContext) -> Self {
        Self { ctx }
    }

    /// Applies one finished match's ratings inside the caller's transaction,
    /// returning the deltas.
    ///
    /// The caller must already have claimed the match's one-shot `rated` flag;
    /// this method assumes it is the only writer for this match. Rating inputs
    /// come from the snapshot taken at match start where available, so a rating
    /// change elsewhere cannot alter this match's arithmetic.
    ///
    /// Returns an empty vector for a cancelled match or one with fewer than two
    /// rateable players.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Domain`] if the adapter rejects the roster, or
    /// [`ServiceError::Database`] if a query fails.
    pub async fn apply_match_result(
        &self,
        tx: &mut Tx<'_>,
        loaded: &LoadedMatch,
        outcome: ReportOutcome,
        channel: &ChannelConfigRow,
    ) -> ServiceResult<Vec<RatingDelta>> {
        let rating_outcome = match outcome {
            ReportOutcome::Win(team) => MatchOutcome::Winner(team),
            ReportOutcome::Draw => MatchOutcome::Draw,
            // Cancelled matches are never rated; the caller should not have
            // reached this point, but returning early is safer than guessing.
            ReportOutcome::Cancel => return Ok(Vec::new()),
        };

        let config = &channel.settings.rating;
        let pool = loaded.info.rating_pool;
        let roster = loaded.roster();
        let stats = self
            .ctx
            .store
            .player_stats_bulk(pool, &roster, config)
            .await?;

        // Build the rating inputs, preferring the snapshot taken at match start
        // so a rating change elsewhere cannot alter this match's arithmetic.
        let mut inputs = Vec::with_capacity(stats.len());
        for row in &stats {
            let Some(player) = loaded.players.iter().find(|p| p.user == row.user) else {
                continue;
            };
            let Some(team) = player.team else {
                // Unassigned players (no-teams queues) cannot be rated.
                continue;
            };
            inputs.push(PlayerRatingState {
                user: row.user,
                team: team as usize,
                rating: player.rating_before.unwrap_or(row.rating),
                deviation: player.deviation_before.unwrap_or(row.deviation),
                volatility: row.volatility,
                streak: row.streak,
            });
        }

        if inputs.len() < 2 {
            return Ok(Vec::new());
        }

        let system = config.system.build();
        let deltas = system.rate(&inputs, rating_outcome, config)?;
        let now = self.ctx.now();

        for delta in &deltas {
            let Some(current) = stats.iter().find(|row| row.user == delta.user) else {
                continue;
            };
            let team = inputs
                .iter()
                .find(|input| input.user == delta.user)
                .map_or(0, |input| input.team);

            let mut updated = current.clone();
            updated.rating = delta.rating_after;
            updated.deviation = delta.deviation_after;
            updated.volatility = delta.volatility_after;
            updated.last_ranked_match_at = Some(now);
            match rating_outcome {
                MatchOutcome::Draw => {
                    updated.draws += 1;
                    updated.streak = 0;
                }
                MatchOutcome::Winner(winner) if winner == team => {
                    updated.wins += 1;
                    updated.streak = updated.streak.max(0) + 1;
                }
                MatchOutcome::Winner(_) => {
                    updated.losses += 1;
                    updated.streak = updated.streak.min(0) - 1;
                }
            }
            self.ctx.store.upsert_player_stats(tx, &updated).await?;
            self.ctx
                .store
                .record_rating_change(
                    tx,
                    pool,
                    Some(loaded.info.id),
                    delta,
                    &format!("match #{}", loaded.info.id),
                    None,
                )
                .await?;
        }

        Ok(deltas)
    }

    /// A player's standing, as `/rank` renders it.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::NoRatingData`] if the player has never played in
    /// this channel's pool, or [`ServiceError::Database`].
    pub async fn rank(&self, channel: &ChannelConfigRow, user: UserId) -> ServiceResult<RankView> {
        let pool = channel.rating_pool();
        let stats = self
            .ctx
            .store
            .player_stats(pool, user)
            .await?
            .ok_or(ServiceError::NoRatingData(channel.channel))?;
        let position = self.ctx.store.leaderboard_position(pool, user).await?;
        let rank = channel.settings.rank_for(stats.rating).cloned();
        let last_change = self
            .ctx
            .store
            .rating_history(pool, user, 1)
            .await?
            .first()
            .map(|row| row.rating_after - row.rating_before);
        Ok(RankView {
            stats,
            position,
            rank,
            last_change,
        })
    }

    /// One page of the leaderboard, plus the total number of qualifying
    /// players. Pages are one-based.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`] if a query fails.
    pub async fn leaderboard(
        &self,
        channel: &ChannelConfigRow,
        page: i64,
        page_size: i64,
    ) -> ServiceResult<(Vec<PlayerStatsRow>, i64)> {
        let page = page.max(1);
        self.ctx
            .store
            .leaderboard(
                channel.rating_pool(),
                channel.settings.leaderboard_min_matches,
                channel.settings.leaderboard_activity_days,
                (page - 1) * page_size,
                page_size,
            )
            .await
    }

    /// The players with the most matches, for `/top`.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`] if the query fails.
    pub async fn most_active(
        &self,
        channel: &ChannelConfigRow,
        limit: i64,
    ) -> ServiceResult<Vec<PlayerStatsRow>> {
        self.ctx
            .store
            .most_active(channel.rating_pool(), limit)
            .await
    }

    // ---------------------------------------------------- administration

    /// Sets a player's rating and deviation outright, writing a history row.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`] if a query fails.
    pub async fn seed(
        &self,
        channel: &ChannelConfigRow,
        actor: UserId,
        user: UserId,
        rating: f64,
        deviation: Option<f64>,
    ) -> ServiceResult<RatingDelta> {
        let pool = channel.rating_pool();
        let config = &channel.settings.rating;
        let current = self
            .ctx
            .store
            .player_stats(pool, user)
            .await?
            .unwrap_or_else(|| PlayerStatsRow::new(pool, user, config));

        let delta = RatingDelta {
            user,
            rating_before: current.rating,
            rating_after: rating,
            deviation_before: current.deviation,
            deviation_after: deviation
                .unwrap_or(current.deviation)
                .clamp(config.min_deviation, config.max_deviation),
            volatility_after: current.volatility,
        };
        self.write_adjustment(pool, current, &delta, "seed", actor)
            .await?;
        self.ctx
            .audit(
                Some(channel.guild),
                Some(channel.channel),
                Some(actor),
                "rating.seed",
                Some(&user.to_string()),
                serde_json::json!({ "rating": rating }),
            )
            .await;
        Ok(delta)
    }

    /// Applies a signed rating penalty, or bonus, with a stated reason.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::NoRatingData`] if the player has no record, or
    /// [`ServiceError::Database`].
    pub async fn penalty(
        &self,
        channel: &ChannelConfigRow,
        actor: UserId,
        user: UserId,
        amount: f64,
        reason: &str,
    ) -> ServiceResult<RatingDelta> {
        let pool = channel.rating_pool();
        let current = self
            .ctx
            .store
            .player_stats(pool, user)
            .await?
            .ok_or(ServiceError::NoRatingData(channel.channel))?;
        let delta = RatingDelta {
            user,
            rating_before: current.rating,
            rating_after: current.rating + amount,
            deviation_before: current.deviation,
            deviation_after: current.deviation,
            volatility_after: current.volatility,
        };
        self.write_adjustment(pool, current, &delta, reason, actor)
            .await?;
        self.ctx
            .audit(
                Some(channel.guild),
                Some(channel.channel),
                Some(actor),
                "rating.penalty",
                Some(&user.to_string()),
                serde_json::json!({ "amount": amount, "reason": reason }),
            )
            .await;
        Ok(delta)
    }

    /// Snaps every player's rating to the floor of the rank they hold. Returns
    /// how many changed.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Rejected`] if the channel has no rank tiers, or
    /// [`ServiceError::Database`].
    ///
    /// Used after retuning rank thresholds so nobody sits fractionally below
    /// the tier they were shown.
    pub async fn snap_to_rank_floors(
        &self,
        channel: &ChannelConfigRow,
        actor: UserId,
    ) -> ServiceResult<usize> {
        if channel.settings.ranks.is_empty() {
            return Err(ServiceError::Rejected(
                "this channel has no rank tiers configured".to_string(),
            ));
        }
        let pool = channel.rating_pool();
        let (players, _) = self.ctx.store.leaderboard(pool, 0, 0, 0, 10_000).await?;
        let mut changed = 0;
        for current in players {
            let Some(tier) = channel.settings.rank_for(current.rating) else {
                continue;
            };
            let floor = f64::from(tier.rating_floor);
            if (current.rating - floor).abs() < f64::EPSILON {
                continue;
            }
            let delta = RatingDelta {
                user: current.user,
                rating_before: current.rating,
                rating_after: floor,
                deviation_before: current.deviation,
                deviation_after: current.deviation,
                volatility_after: current.volatility,
            };
            self.write_adjustment(pool, current, &delta, "snap to rank floor", actor)
                .await?;
            changed += 1;
        }
        self.ctx
            .audit(
                Some(channel.guild),
                Some(channel.channel),
                Some(actor),
                "rating.snap",
                None,
                serde_json::json!({ "changed": changed }),
            )
            .await;
        Ok(changed)
    }

    /// Hides or unhides a player on the leaderboard.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::NoRatingData`] if the player has no record, or
    /// [`ServiceError::Database`].
    pub async fn set_hidden(
        &self,
        channel: &ChannelConfigRow,
        actor: UserId,
        user: UserId,
        hidden: bool,
    ) -> ServiceResult<()> {
        let changed = self
            .ctx
            .store
            .set_player_hidden(channel.rating_pool(), user, hidden)
            .await?;
        if !changed {
            return Err(ServiceError::NoRatingData(channel.channel));
        }
        self.ctx
            .audit(
                Some(channel.guild),
                Some(channel.channel),
                Some(actor),
                if hidden {
                    "rating.hide"
                } else {
                    "rating.unhide"
                },
                Some(&user.to_string()),
                serde_json::json!({}),
            )
            .await;
        Ok(())
    }

    /// Deletes every rating and history row in the channel's pool. Returns how
    /// many player records were removed.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`] if a query fails.
    pub async fn reset_channel(
        &self,
        channel: &ChannelConfigRow,
        actor: UserId,
    ) -> ServiceResult<u64> {
        let removed = self
            .ctx
            .store
            .reset_channel_stats(channel.rating_pool())
            .await?;
        self.ctx
            .audit(
                Some(channel.guild),
                Some(channel.channel),
                Some(actor),
                "stats.reset",
                None,
                serde_json::json!({ "players": removed }),
            )
            .await;
        Ok(removed)
    }

    /// Deletes one player's rating and history.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::NoRatingData`] if they had none, or
    /// [`ServiceError::Database`].
    pub async fn reset_player(
        &self,
        channel: &ChannelConfigRow,
        actor: UserId,
        user: UserId,
    ) -> ServiceResult<()> {
        let removed = self
            .ctx
            .store
            .reset_player_stats(channel.rating_pool(), user)
            .await?;
        if !removed {
            return Err(ServiceError::NoRatingData(channel.channel));
        }
        self.ctx
            .audit(
                Some(channel.guild),
                Some(channel.channel),
                Some(actor),
                "stats.reset_player",
                Some(&user.to_string()),
                serde_json::json!({}),
            )
            .await;
        Ok(())
    }

    /// Moves a rating record onto another account, merging if the target
    /// already has one. History follows the record.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Rejected`] if both accounts are the same, or
    /// [`ServiceError::Database`].
    pub async fn replace_player(
        &self,
        channel: &ChannelConfigRow,
        actor: UserId,
        from: UserId,
        into: UserId,
    ) -> ServiceResult<()> {
        if from == into {
            return Err(ServiceError::Rejected(
                "a player cannot replace themselves".to_string(),
            ));
        }
        self.ctx
            .store
            .replace_player(channel.rating_pool(), from, into)
            .await?;
        self.ctx
            .audit(
                Some(channel.guild),
                Some(channel.channel),
                Some(actor),
                "stats.replace_player",
                Some(&from.to_string()),
                serde_json::json!({ "into": into.get() }),
            )
            .await;
        Ok(())
    }

    /// Applies inactivity decay across a channel. Returns how many players
    /// moved.
    ///
    /// Decay does not reset the inactivity clock, so a player continues to
    /// decay while they stay away; a player already at the floor is left alone,
    /// which keeps repeated runs cheap and idempotent in effect.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`] if a query fails.
    pub async fn apply_decay(&self, channel: &ChannelConfigRow) -> ServiceResult<usize> {
        let config = &channel.settings.rating;
        if config.inactivity_decay_per_day <= 0.0 && config.deviation_decay_per_day <= 0.0 {
            return Ok(0);
        }
        let now = self.ctx.now();
        let pool = channel.rating_pool();
        let players = self
            .ctx
            .store
            .inactive_players(pool, now - Duration::days(1))
            .await?;

        let mut changed = 0;
        for current in players {
            let Some(last) = current.last_ranked_match_at else {
                continue;
            };
            let days = (now - last).num_seconds() as f64 / 86_400.0;
            let (rating, deviation) = apply_decay(current.rating, current.deviation, days, config);
            if (rating - current.rating).abs() < 1e-9
                && (deviation - current.deviation).abs() < 1e-9
            {
                continue;
            }
            let delta = RatingDelta {
                user: current.user,
                rating_before: current.rating,
                rating_after: rating,
                deviation_before: current.deviation,
                deviation_after: deviation,
                volatility_after: current.volatility,
            };
            // Decay must not reset the inactivity clock, or a player would
            // decay once and never again.
            let mut updated = current.clone();
            updated.rating = rating;
            updated.deviation = deviation;
            let mut tx = self.ctx.store.begin().await?;
            self.ctx
                .store
                .upsert_player_stats(&mut tx, &updated)
                .await?;
            self.ctx
                .store
                .record_rating_change(&mut tx, pool, None, &delta, "inactivity decay", None)
                .await?;
            tx.commit().await?;
            changed += 1;
        }
        Ok(changed)
    }

    /// Writes a manual adjustment and its history row in one transaction.
    async fn write_adjustment(
        &self,
        pool: ChannelId,
        current: PlayerStatsRow,
        delta: &RatingDelta,
        reason: &str,
        actor: UserId,
    ) -> ServiceResult<()> {
        let mut updated = current;
        updated.rating = delta.rating_after;
        updated.deviation = delta.deviation_after;
        let mut tx = self.ctx.store.begin().await?;
        self.ctx
            .store
            .upsert_player_stats(&mut tx, &updated)
            .await?;
        self.ctx
            .store
            .record_rating_change(&mut tx, pool, None, delta, reason, Some(actor))
            .await?;
        tx.commit().await?;
        Ok(())
    }
}

/// The nickname prefix a rank grants: its emoji and rating, or the bare
/// rating in brackets when no tier applies.
pub fn nickname_prefix(rank: Option<&RankTier>, rating: f64, system: RatingSystemKind) -> String {
    let rating = match system {
        // TrueSkill's mean alone overstates a new player; the leaderboards in
        // the literature show the conservative estimate instead.
        RatingSystemKind::TrueSkill => rating,
        _ => rating,
    };
    match rank {
        Some(tier) => match &tier.emoji {
            Some(emoji) if !emoji.is_empty() => format!("{emoji}{} | ", rating.round() as i64),
            _ => format!("[{}] ", rating.round() as i64),
        },
        None => format!("[{}] ", rating.round() as i64),
    }
}

/// Truncates a nickname to Discord's 32-character limit without splitting a
/// multi-byte character.
pub fn fit_nickname(prefix: &str, name: &str) -> String {
    const LIMIT: usize = 32;
    let mut result = format!("{prefix}{name}");
    if result.chars().count() <= LIMIT {
        return result;
    }
    result = result.chars().take(LIMIT).collect();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::ids::RoleId;

    #[test]
    fn nickname_prefix_uses_the_rank_emoji_when_present() {
        let tier = RankTier {
            rating_floor: 1500,
            name: "Gold".into(),
            emoji: Some("🥇".into()),
            role_id: Some(RoleId(1)),
        };
        assert_eq!(
            nickname_prefix(Some(&tier), 1612.4, RatingSystemKind::Flat),
            "🥇1612 | "
        );
    }

    #[test]
    fn nickname_prefix_falls_back_to_the_bare_rating() {
        assert_eq!(
            nickname_prefix(None, 1499.6, RatingSystemKind::Flat),
            "[1500] "
        );
    }

    #[test]
    fn nicknames_are_truncated_to_the_discord_limit() {
        let fitted = fit_nickname("[1500] ", &"a".repeat(60));
        assert_eq!(fitted.chars().count(), 32);
        assert!(fitted.starts_with("[1500] "));
    }

    #[test]
    fn truncation_never_splits_a_multi_byte_character() {
        let fitted = fit_nickname("🥇1612 | ", &"日本語".repeat(20));
        assert!(fitted.chars().count() <= 32);
        // Round-tripping proves no partial code point survived.
        assert_eq!(
            fitted,
            String::from_utf8(fitted.clone().into_bytes()).unwrap()
        );
    }

    #[test]
    fn a_short_nickname_is_left_alone() {
        assert_eq!(fit_nickname("[1500] ", "ada"), "[1500] ada");
    }
}
