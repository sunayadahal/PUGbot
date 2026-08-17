//! Flat rating changes: every win is worth the same, before scaling.
//!
//! This is the simplest system to explain in a Discord channel and the default
//! for new channels. Deviation is still tracked so a channel can migrate to
//! Glicko-2 later without losing the notion of uncertainty; it contracts
//! geometrically toward `min_deviation` as a player accumulates matches.

use super::{
    streak_multiplier, validate_roster, MatchOutcome, PlayerRatingState, RatingConfig, RatingDelta,
    RatingSystem, RatingSystemKind,
};
use crate::error::DomainResult;

/// Fraction of the gap to `min_deviation` removed per rated match.
const DEVIATION_CONTRACTION: f64 = 0.1;

/// The flat rating adapter. See the [module documentation](self).
#[derive(Debug, Clone, Copy, Default)]
pub struct Flat;

impl RatingSystem for Flat {
    fn kind(&self) -> RatingSystemKind {
        RatingSystemKind::Flat
    }

    fn rate(
        &self,
        players: &[PlayerRatingState],
        outcome: MatchOutcome,
        config: &RatingConfig,
    ) -> DomainResult<Vec<RatingDelta>> {
        validate_roster(players, outcome)?;

        let deltas = players
            .iter()
            .map(|player| {
                let change = match outcome {
                    MatchOutcome::Draw => config.draw_bonus,
                    MatchOutcome::Winner(winner) if winner == player.team => {
                        config.scale
                            * config.win_scale
                            * streak_multiplier(config, player.streak, true)
                    }
                    MatchOutcome::Winner(_) => {
                        -config.scale
                            * config.loss_scale
                            * streak_multiplier(config, player.streak, false)
                    }
                };
                let deviation_after = (player.deviation
                    - (player.deviation - config.min_deviation) * DEVIATION_CONTRACTION)
                    .clamp(config.min_deviation, config.max_deviation);
                RatingDelta {
                    user: player.user,
                    rating_before: player.rating,
                    rating_after: player.rating + change,
                    deviation_before: player.deviation,
                    deviation_after,
                    volatility_after: player.volatility,
                }
            })
            .collect();
        Ok(deltas)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::ids::UserId;
    use crate::domain::rating::StreakConfig;

    fn roster() -> Vec<PlayerRatingState> {
        vec![
            PlayerRatingState {
                user: UserId(1),
                team: 0,
                rating: 1500.0,
                deviation: 200.0,
                volatility: 0.06,
                streak: 0,
            },
            PlayerRatingState {
                user: UserId(2),
                team: 1,
                rating: 1500.0,
                deviation: 200.0,
                volatility: 0.06,
                streak: 0,
            },
        ]
    }

    #[test]
    fn win_and_loss_are_symmetric_by_default() {
        let config = RatingConfig::default();
        let deltas = Flat
            .rate(&roster(), MatchOutcome::Winner(0), &config)
            .unwrap();
        assert_eq!(deltas[0].change(), config.scale);
        assert_eq!(deltas[1].change(), -config.scale);
    }

    #[test]
    fn win_and_loss_scaling_are_applied_independently() {
        let config = RatingConfig {
            scale: 20.0,
            win_scale: 1.5,
            loss_scale: 0.5,
            ..Default::default()
        };
        let deltas = Flat
            .rate(&roster(), MatchOutcome::Winner(0), &config)
            .unwrap();
        assert_eq!(deltas[0].change(), 30.0);
        assert_eq!(deltas[1].change(), -10.0);
    }

    #[test]
    fn a_draw_applies_the_draw_bonus_to_both_sides() {
        let config = RatingConfig {
            draw_bonus: 3.0,
            ..Default::default()
        };
        let deltas = Flat.rate(&roster(), MatchOutcome::Draw, &config).unwrap();
        assert_eq!(deltas[0].change(), 3.0);
        assert_eq!(deltas[1].change(), 3.0);
    }

    #[test]
    fn a_winning_streak_amplifies_only_the_winner() {
        let config = RatingConfig {
            scale: 20.0,
            win_streak: Some(StreakConfig {
                min_streak: 2,
                step: 0.5,
                max_multiplier: 3.0,
            }),
            ..Default::default()
        };
        let mut players = roster();
        players[0].streak = 3; // on a 3-win run
        players[1].streak = 2; // on a 2-win run, but about to lose
        let deltas = Flat
            .rate(&players, MatchOutcome::Winner(0), &config)
            .unwrap();
        assert_eq!(deltas[0].change(), 20.0 * 2.0);
        assert_eq!(
            deltas[1].change(),
            -20.0,
            "a win streak must not amplify a loss"
        );
    }

    #[test]
    fn deviation_contracts_toward_the_minimum_but_never_below() {
        let config = RatingConfig::default();
        let mut players = roster();
        players[0].deviation = config.min_deviation;
        let deltas = Flat
            .rate(&players, MatchOutcome::Winner(0), &config)
            .unwrap();
        assert_eq!(deltas[0].deviation_after, config.min_deviation);
        assert!(deltas[1].deviation_after < deltas[1].deviation_before);
        assert!(deltas[1].deviation_after > config.min_deviation);
    }

    #[test]
    fn rating_changes_are_zero_sum_for_symmetric_teams() {
        let config = RatingConfig::default();
        let players = vec![
            PlayerRatingState {
                user: UserId(1),
                team: 0,
                rating: 1500.0,
                deviation: 100.0,
                volatility: 0.06,
                streak: 0,
            },
            PlayerRatingState {
                user: UserId(2),
                team: 0,
                rating: 1000.0,
                deviation: 100.0,
                volatility: 0.06,
                streak: 0,
            },
            PlayerRatingState {
                user: UserId(3),
                team: 1,
                rating: 2000.0,
                deviation: 100.0,
                volatility: 0.06,
                streak: 0,
            },
            PlayerRatingState {
                user: UserId(4),
                team: 1,
                rating: 1200.0,
                deviation: 100.0,
                volatility: 0.06,
                streak: 0,
            },
        ];
        let deltas = Flat
            .rate(&players, MatchOutcome::Winner(1), &config)
            .unwrap();
        let total: f64 = deltas.iter().map(RatingDelta::change).sum();
        assert!(
            total.abs() < 1e-9,
            "flat ratings should not inflate: {total}"
        );
    }
}
