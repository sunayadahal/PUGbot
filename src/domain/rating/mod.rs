//! Rating systems, rating configuration, and decay.
//!
//! Every adapter implements [`RatingSystem`] and is a pure function of the
//! roster plus the outcome, so a match can be re-rated deterministically and
//! the adapters can be unit-tested without a database.

pub mod flat;
pub mod glicko2;
pub mod math;
pub mod trueskill;

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::domain::ids::UserId;
use crate::error::{DomainError, DomainResult};

/// Which rating algorithm a channel uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RatingSystemKind {
    /// A fixed change per result. See [`flat`].
    Flat,
    /// Glickman's Glicko-2, tracking rating, deviation and volatility. See
    /// [`glicko2`].
    Glicko2,
    /// Microsoft's TrueSkill, tracking a Gaussian per player. See
    /// [`trueskill`].
    TrueSkill,
}

impl RatingSystemKind {
    /// Every implemented system, for exhaustive tests and choice lists.
    pub const ALL: [RatingSystemKind; 3] = [
        RatingSystemKind::Flat,
        RatingSystemKind::Glicko2,
        RatingSystemKind::TrueSkill,
    ];

    /// The stable string stored in the channel settings.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            RatingSystemKind::Flat => "flat",
            RatingSystemKind::Glicko2 => "glicko2",
            RatingSystemKind::TrueSkill => "trueskill",
        }
    }

    /// Parses the stored form, the inverse of
    /// [`RatingSystemKind::as_str`].
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        RatingSystemKind::ALL
            .into_iter()
            .find(|kind| kind.as_str() == value)
    }

    /// Instantiates the adapter for this system.
    #[must_use]
    pub fn build(self) -> Arc<dyn RatingSystem> {
        match self {
            RatingSystemKind::Flat => Arc::new(flat::Flat),
            RatingSystemKind::Glicko2 => Arc::new(glicko2::Glicko2),
            RatingSystemKind::TrueSkill => Arc::new(trueskill::TrueSkill),
        }
    }
}

/// Extra reward or punishment for players on a run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StreakConfig {
    /// Streak length at which the multiplier starts applying.
    pub min_streak: i32,
    /// Added to the multiplier per match beyond `min_streak`.
    pub step: f64,
    /// Ceiling on the multiplier, so a long run cannot run away.
    pub max_multiplier: f64,
}

impl Default for StreakConfig {
    fn default() -> Self {
        Self {
            min_streak: 3,
            step: 0.1,
            max_multiplier: 1.5,
        }
    }
}

impl StreakConfig {
    /// Multiplier for a player whose current streak is `streak` (positive for
    /// wins, negative for losses) and who is about to extend it.
    /// The multiplier for a player on `streak`.
    ///
    /// The sign of `streak` is ignored; the caller decides whether a winning or
    /// losing streak is the relevant one.
    #[must_use]
    pub fn multiplier(&self, streak: i32) -> f64 {
        let length = streak.abs();
        if length < self.min_streak {
            return 1.0;
        }
        let steps = f64::from(length - self.min_streak + 1);
        (1.0 + steps * self.step).min(self.max_multiplier)
    }
}

/// A channel's rating parameters.
///
/// The defaults describe a plain flat system; the Glicko-2 and TrueSkill fields
/// are ignored unless the matching system is selected.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RatingConfig {
    /// Which algorithm rates matches in this channel.
    pub system: RatingSystemKind,
    /// Rating given to a player who has never played here.
    pub initial_rating: f64,
    /// Deviation given to a player who has never played here.
    pub initial_deviation: f64,
    /// Floor on deviation: how certain the system is ever allowed to become.
    pub min_deviation: f64,
    /// Ceiling on deviation, which inactivity decay grows back toward.
    pub max_deviation: f64,
    /// Glicko-2 volatility for a new player.
    pub initial_volatility: f64,
    /// Overall multiplier applied on top of the win/loss scaling.
    pub scale: f64,
    /// Multiplier applied to a win.
    pub win_scale: f64,
    /// Multiplier applied to a loss.
    pub loss_scale: f64,
    /// Applied to both sides of a draw; may be negative.
    pub draw_bonus: f64,
    /// Extra scaling on winning streaks, when configured.
    pub win_streak: Option<StreakConfig>,
    /// Extra scaling on losing streaks, when configured.
    pub loss_streak: Option<StreakConfig>,
    /// Rating points shed per inactive day, floored at `initial_rating`.
    pub inactivity_decay_per_day: f64,
    /// Deviation regained per inactive day, capped at `max_deviation`.
    pub deviation_decay_per_day: f64,
    /// Glicko-2 system constant (volatility change rate).
    pub tau: f64,
    /// TrueSkill performance variance: how much a single match's outcome is
    /// attributed to chance.
    pub beta: f64,
    /// TrueSkill dynamics factor: uncertainty added back before each match so
    /// ratings never freeze.
    pub trueskill_tau: f64,
    /// TrueSkill assumed draw probability, which sets the draw margin.
    pub draw_probability: f64,
}

impl Default for RatingConfig {
    fn default() -> Self {
        Self {
            system: RatingSystemKind::Flat,
            initial_rating: 1500.0,
            initial_deviation: 200.0,
            min_deviation: 50.0,
            max_deviation: 350.0,
            initial_volatility: 0.06,
            scale: 25.0,
            win_scale: 1.0,
            loss_scale: 1.0,
            draw_bonus: 0.0,
            win_streak: None,
            loss_streak: None,
            inactivity_decay_per_day: 0.0,
            deviation_decay_per_day: 0.0,
            tau: 0.5,
            beta: 250.0,
            trueskill_tau: 2.5,
            draw_probability: 0.1,
        }
    }
}

impl RatingConfig {
    /// Rejects a rating configuration that cannot produce sane results.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::InvalidConfig`] for non-positive deviations, a
    /// minimum above the initial value, a maximum below it, negative scales or
    /// decay rates, a non-positive `tau` or `beta`, or a draw probability
    /// outside `[0, 1)`.
    pub fn validate(&self) -> DomainResult<()> {
        if self.initial_deviation <= 0.0 || self.min_deviation <= 0.0 {
            return Err(DomainError::InvalidConfig(
                "rating deviations must be positive".into(),
            ));
        }
        if self.min_deviation > self.initial_deviation {
            return Err(DomainError::InvalidConfig(
                "minimum deviation cannot exceed the initial deviation".into(),
            ));
        }
        if self.max_deviation < self.initial_deviation {
            return Err(DomainError::InvalidConfig(
                "maximum deviation cannot be below the initial deviation".into(),
            ));
        }
        if self.scale < 0.0 || self.win_scale < 0.0 || self.loss_scale < 0.0 {
            return Err(DomainError::InvalidConfig(
                "rating scales cannot be negative".into(),
            ));
        }
        if self.tau <= 0.0 {
            return Err(DomainError::InvalidConfig(
                "glicko-2 tau must be positive".into(),
            ));
        }
        if self.beta <= 0.0 {
            return Err(DomainError::InvalidConfig(
                "trueskill beta must be positive".into(),
            ));
        }
        if !(0.0..1.0).contains(&self.draw_probability) {
            return Err(DomainError::InvalidConfig(
                "draw probability must be in [0, 1)".into(),
            ));
        }
        if self.inactivity_decay_per_day < 0.0 || self.deviation_decay_per_day < 0.0 {
            return Err(DomainError::InvalidConfig(
                "decay rates cannot be negative".into(),
            ));
        }
        Ok(())
    }

    /// Rating and deviation for a player who has never played here.
    #[must_use]
    pub fn initial_state(&self, user: UserId, team: usize) -> PlayerRatingState {
        PlayerRatingState {
            user,
            team,
            rating: self.initial_rating,
            deviation: self.initial_deviation,
            volatility: self.initial_volatility,
            streak: 0,
        }
    }
}

/// A player's rating inputs for one match.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlayerRatingState {
    /// Who is being rated.
    pub user: UserId,
    /// The team they played on.
    pub team: usize,
    /// Rating before the match.
    pub rating: f64,
    /// Uncertainty before the match.
    pub deviation: f64,
    /// Glicko-2 volatility before the match. Unused by the other systems.
    pub volatility: f64,
    /// Positive for a winning streak, negative for a losing streak.
    pub streak: i32,
}

/// The outcome a rating adapter is asked to apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchOutcome {
    /// The team at this index won.
    Winner(usize),
    /// Nobody won.
    Draw,
}

impl MatchOutcome {
    /// Per-player score in `[0, 1]`, as the rating literature expects.
    /// The score this outcome awards `team`, on the `[0, 1]` scale the rating
    /// literature uses: 1 for a win, 0 for a loss, 0.5 for a draw.
    #[must_use]
    pub fn score_for(self, team: usize) -> f64 {
        match self {
            MatchOutcome::Draw => 0.5,
            MatchOutcome::Winner(winner) if winner == team => 1.0,
            MatchOutcome::Winner(_) => 0.0,
        }
    }
}

/// What a rating adapter produced for one player.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RatingDelta {
    /// The player this applies to.
    pub user: UserId,
    /// Rating before the match.
    pub rating_before: f64,
    /// Rating after the match.
    pub rating_after: f64,
    /// Uncertainty before the match.
    pub deviation_before: f64,
    /// Uncertainty after the match, clamped to the configured bounds.
    pub deviation_after: f64,
    /// Volatility after the match. Unchanged except under Glicko-2.
    pub volatility_after: f64,
}

impl RatingDelta {
    /// The signed rating movement.
    #[must_use]
    pub fn change(&self) -> f64 {
        self.rating_after - self.rating_before
    }
}

/// A rating algorithm. Implementations must be pure: same inputs, same output.
pub trait RatingSystem: Send + Sync + std::fmt::Debug {
    /// Which system this is, for logging and round-tripping configuration.
    fn kind(&self) -> RatingSystemKind;

    /// Rates one finished match.
    ///
    /// Implementations are pure: the same roster, outcome and configuration
    /// always produce the same deltas, so a match can be re-rated
    /// deterministically.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::InvalidConfig`] for an empty roster, a duplicated
    /// player, or fewer than two teams; [`DomainError::NoSuchTeam`] if the
    /// winning team is not on the roster.
    fn rate(
        &self,
        players: &[PlayerRatingState],
        outcome: MatchOutcome,
        config: &RatingConfig,
    ) -> DomainResult<Vec<RatingDelta>>;
}

/// Shared validation for adapters: at least two distinct, non-empty teams and
/// no duplicated players.
pub(crate) fn validate_roster(
    players: &[PlayerRatingState],
    outcome: MatchOutcome,
) -> DomainResult<Vec<usize>> {
    if players.is_empty() {
        return Err(DomainError::InvalidConfig(
            "cannot rate a match with no players".into(),
        ));
    }
    let mut ids: Vec<UserId> = players.iter().map(|p| p.user).collect();
    ids.sort_unstable();
    ids.dedup();
    if ids.len() != players.len() {
        return Err(DomainError::InvalidConfig(
            "a player appears twice in the rated roster".into(),
        ));
    }
    let mut teams: Vec<usize> = players.iter().map(|p| p.team).collect();
    teams.sort_unstable();
    teams.dedup();
    if teams.len() < 2 {
        return Err(DomainError::InvalidConfig(
            "rating requires at least two teams".into(),
        ));
    }
    if let MatchOutcome::Winner(winner) = outcome {
        if !teams.contains(&winner) {
            return Err(DomainError::NoSuchTeam(winner));
        }
    }
    Ok(teams)
}

/// The mean rating and deviation of every player *not* on `team`.
pub(crate) fn opponent_aggregate(players: &[PlayerRatingState], team: usize) -> (f64, f64) {
    let opponents: Vec<&PlayerRatingState> = players.iter().filter(|p| p.team != team).collect();
    let count = opponents.len() as f64;
    let rating = opponents.iter().map(|p| p.rating).sum::<f64>() / count;
    // Deviations combine in quadrature, which keeps an uncertain opponent
    // uncertain rather than averaging the uncertainty away.
    let deviation = (opponents.iter().map(|p| p.deviation.powi(2)).sum::<f64>() / count).sqrt();
    (rating, deviation)
}

/// Effective scaling for a player's result, including streak multipliers.
pub(crate) fn streak_multiplier(config: &RatingConfig, streak: i32, won: bool) -> f64 {
    let applicable = if won {
        config.win_streak.as_ref().filter(|_| streak > 0)
    } else {
        config.loss_streak.as_ref().filter(|_| streak < 0)
    };
    applicable.map_or(1.0, |cfg| cfg.multiplier(streak))
}

/// Applies inactivity decay to a stored rating.
///
/// Returns the decayed `(rating, deviation)` pair.
///
/// Rating decays toward `initial_rating` and never below it, so decay cannot
/// push an inactive player under a newcomer. Deviation grows back toward
/// `max_deviation`, restoring uncertainty about a player who stopped playing.
pub fn apply_decay(
    rating: f64,
    deviation: f64,
    days_inactive: f64,
    config: &RatingConfig,
) -> (f64, f64) {
    if days_inactive <= 0.0 {
        return (rating, deviation);
    }
    let decayed_rating = if config.inactivity_decay_per_day > 0.0 && rating > config.initial_rating
    {
        (rating - config.inactivity_decay_per_day * days_inactive).max(config.initial_rating)
    } else {
        rating
    };
    let decayed_deviation = (deviation + config.deviation_decay_per_day * days_inactive)
        .min(config.max_deviation)
        .max(config.min_deviation);
    (decayed_rating, decayed_deviation)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roster(ratings: &[(usize, f64)]) -> Vec<PlayerRatingState> {
        ratings
            .iter()
            .enumerate()
            .map(|(i, &(team, rating))| PlayerRatingState {
                user: UserId(i as i64 + 1),
                team,
                rating,
                deviation: 200.0,
                volatility: 0.06,
                streak: 0,
            })
            .collect()
    }

    #[test]
    fn every_adapter_rates_a_two_versus_two_win() {
        let players = roster(&[(0, 1500.0), (0, 1600.0), (1, 1400.0), (1, 1550.0)]);
        for kind in RatingSystemKind::ALL {
            let config = RatingConfig {
                system: kind,
                ..Default::default()
            };
            let system = kind.build();
            let deltas = system
                .rate(&players, MatchOutcome::Winner(0), &config)
                .unwrap_or_else(|e| panic!("{kind:?} failed: {e}"));
            assert_eq!(deltas.len(), 4);
            for delta in &deltas {
                let team = players.iter().find(|p| p.user == delta.user).unwrap().team;
                if team == 0 {
                    assert!(delta.change() > 0.0, "{kind:?} did not reward the winner");
                } else {
                    assert!(delta.change() < 0.0, "{kind:?} did not punish the loser");
                }
                assert!(delta.deviation_after >= config.min_deviation);
                assert!(delta.deviation_after <= config.max_deviation);
            }
        }
    }

    #[test]
    fn every_adapter_rejects_a_single_team_or_duplicate_player() {
        let one_team = roster(&[(0, 1500.0), (0, 1500.0)]);
        let mut duplicated = roster(&[(0, 1500.0), (1, 1500.0)]);
        duplicated[1].user = duplicated[0].user;
        for kind in RatingSystemKind::ALL {
            let config = RatingConfig {
                system: kind,
                ..Default::default()
            };
            let system = kind.build();
            assert!(system
                .rate(&one_team, MatchOutcome::Winner(0), &config)
                .is_err());
            assert!(system
                .rate(&duplicated, MatchOutcome::Winner(0), &config)
                .is_err());
            assert!(system.rate(&[], MatchOutcome::Draw, &config).is_err());
        }
    }

    #[test]
    fn every_adapter_returns_one_delta_per_player_with_matching_before_values() {
        let players = roster(&[(0, 1500.0), (1, 1500.0), (0, 1200.0), (1, 1800.0)]);
        for kind in RatingSystemKind::ALL {
            let config = RatingConfig {
                system: kind,
                ..Default::default()
            };
            let deltas = kind
                .build()
                .rate(&players, MatchOutcome::Draw, &config)
                .unwrap();
            assert_eq!(deltas.len(), players.len());
            for player in &players {
                let delta = deltas.iter().find(|d| d.user == player.user).unwrap();
                assert_eq!(delta.rating_before, player.rating);
                assert_eq!(delta.deviation_before, player.deviation);
                assert!(delta.rating_after.is_finite());
                assert!(delta.deviation_after.is_finite());
            }
        }
    }

    #[test]
    fn outcome_scores_follow_the_rating_convention() {
        assert_eq!(MatchOutcome::Winner(0).score_for(0), 1.0);
        assert_eq!(MatchOutcome::Winner(0).score_for(1), 0.0);
        assert_eq!(MatchOutcome::Draw.score_for(1), 0.5);
    }

    #[test]
    fn streak_multiplier_only_applies_past_the_threshold() {
        let cfg = StreakConfig {
            min_streak: 3,
            step: 0.1,
            max_multiplier: 1.5,
        };
        assert_eq!(cfg.multiplier(2), 1.0);
        assert!((cfg.multiplier(3) - 1.1).abs() < 1e-9);
        assert!((cfg.multiplier(5) - 1.3).abs() < 1e-9);
        assert_eq!(cfg.multiplier(50), 1.5, "capped");
        assert_eq!(cfg.multiplier(-4), cfg.multiplier(4), "sign agnostic");
    }

    #[test]
    fn streak_multiplier_matches_the_direction_of_the_result() {
        let config = RatingConfig {
            win_streak: Some(StreakConfig::default()),
            loss_streak: Some(StreakConfig::default()),
            ..Default::default()
        };
        assert!(streak_multiplier(&config, 5, true) > 1.0);
        assert_eq!(
            streak_multiplier(&config, 5, false),
            1.0,
            "a winning streak must not amplify a loss"
        );
        assert!(streak_multiplier(&config, -5, false) > 1.0);
        assert_eq!(streak_multiplier(&config, -5, true), 1.0);
    }

    #[test]
    fn decay_pulls_toward_the_initial_rating_and_never_past_it() {
        let config = RatingConfig {
            inactivity_decay_per_day: 5.0,
            deviation_decay_per_day: 2.0,
            ..Default::default()
        };
        let (rating, deviation) = apply_decay(1700.0, 60.0, 10.0, &config);
        assert_eq!(rating, 1650.0);
        assert_eq!(deviation, 80.0);

        let (rating, _) = apply_decay(1520.0, 60.0, 100.0, &config);
        assert_eq!(
            rating, config.initial_rating,
            "decay floors at the baseline"
        );

        let (rating, _) = apply_decay(1000.0, 60.0, 100.0, &config);
        assert_eq!(rating, 1000.0, "below-average players are not decayed");
    }

    #[test]
    fn decay_caps_deviation_at_the_maximum() {
        let config = RatingConfig {
            deviation_decay_per_day: 50.0,
            ..Default::default()
        };
        let (_, deviation) = apply_decay(1500.0, 100.0, 365.0, &config);
        assert_eq!(deviation, config.max_deviation);
    }

    #[test]
    fn rating_system_kinds_round_trip() {
        for kind in RatingSystemKind::ALL {
            assert_eq!(RatingSystemKind::parse(kind.as_str()), Some(kind));
            assert_eq!(kind.build().kind(), kind);
        }
        assert_eq!(RatingSystemKind::parse("elo"), None);
    }
}
