//! TrueSkill adapter for two-team matches.
//!
//! Ratings are stored as a Gaussian: `rating` is μ and `deviation` is σ. This
//! is the standard closed-form two-team update — the full factor-graph message
//! passing is only needed for three or more teams, so rosters with more teams
//! are rated pairwise against the combined field.

use super::{
    math, streak_multiplier, validate_roster, MatchOutcome, PlayerRatingState, RatingConfig,
    RatingDelta, RatingSystem, RatingSystemKind,
};
use crate::error::DomainResult;

/// The TrueSkill rating adapter. See the [module documentation](self).
#[derive(Debug, Clone, Copy, Default)]
pub struct TrueSkill;

impl RatingSystem for TrueSkill {
    fn kind(&self) -> RatingSystemKind {
        RatingSystemKind::TrueSkill
    }

    fn rate(
        &self,
        players: &[PlayerRatingState],
        outcome: MatchOutcome,
        config: &RatingConfig,
    ) -> DomainResult<Vec<RatingDelta>> {
        let teams = validate_roster(players, outcome)?;

        // With more than two teams, each team is rated against the union of
        // the others; the two-team case reduces to the textbook update.
        let mut results = Vec::with_capacity(players.len());
        for player in players {
            let own: Vec<&PlayerRatingState> =
                players.iter().filter(|p| p.team == player.team).collect();
            let others: Vec<&PlayerRatingState> =
                players.iter().filter(|p| p.team != player.team).collect();

            let beta = config.beta;
            let tau = config.trueskill_tau;

            // Dynamics: a little uncertainty is added back before each match so
            // ratings never freeze completely.
            let sigma = (player.deviation * player.deviation + tau * tau).sqrt();

            let mu_own: f64 = own.iter().map(|p| p.rating).sum();
            let mu_others: f64 = others.iter().map(|p| p.rating).sum();
            let variance_sum: f64 = own
                .iter()
                .chain(others.iter())
                .map(|p| p.deviation * p.deviation + tau * tau)
                .sum();
            let total_players = (own.len() + others.len()) as f64;
            let c = (variance_sum + total_players * beta * beta).sqrt();

            let epsilon = draw_margin(config.draw_probability, total_players, beta);

            let won = matches!(outcome, MatchOutcome::Winner(w) if w == player.team);
            let drawn = outcome == MatchOutcome::Draw;

            // `t` is always written from the perspective of the winning side so
            // the win formulas apply unchanged; the sign is restored afterwards.
            let (t, sign) = if drawn || won {
                (mu_own - mu_others, 1.0)
            } else {
                (mu_others - mu_own, -1.0)
            };

            let (v, w) = if drawn {
                (
                    math::v_draw(t / c, epsilon / c),
                    math::w_draw(t / c, epsilon / c),
                )
            } else {
                (
                    math::v_win(t / c, epsilon / c),
                    math::w_win(t / c, epsilon / c),
                )
            };

            let mean_multiplier = sigma * sigma / c;
            let variance_multiplier = (sigma * sigma) / (c * c);

            let raw_change = sign * mean_multiplier * v;
            let sigma_after = (sigma * sigma * (1.0 - variance_multiplier * w))
                .max(1e-6)
                .sqrt();

            let adjusted_change = match outcome {
                MatchOutcome::Draw => raw_change + config.draw_bonus,
                MatchOutcome::Winner(winner) if winner == player.team => {
                    raw_change * config.win_scale * streak_multiplier(config, player.streak, true)
                }
                MatchOutcome::Winner(_) => {
                    raw_change * config.loss_scale * streak_multiplier(config, player.streak, false)
                }
            };

            results.push(RatingDelta {
                user: player.user,
                rating_before: player.rating,
                rating_after: player.rating + adjusted_change,
                deviation_before: player.deviation,
                deviation_after: sigma_after.clamp(config.min_deviation, config.max_deviation),
                volatility_after: player.volatility,
            });
        }
        debug_assert_eq!(results.len(), players.len());
        debug_assert!(teams.len() >= 2);
        Ok(results)
    }
}

/// The performance gap below which TrueSkill considers a match drawn.
fn draw_margin(draw_probability: f64, total_players: f64, beta: f64) -> f64 {
    if draw_probability <= 0.0 {
        return 0.0;
    }
    math::inv_cdf((draw_probability + 1.0) / 2.0) * total_players.sqrt() * beta
}

/// The rating shown to players: a conservative estimate that only rises once
/// the system is confident, which is what TrueSkill leaderboards display.
///
/// This is `mu - 3 * sigma`, the value a new player must earn their way up to.
pub fn conservative_rating(mu: f64, sigma: f64) -> f64 {
    mu - 3.0 * sigma
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::ids::UserId;

    fn config() -> RatingConfig {
        RatingConfig {
            system: RatingSystemKind::TrueSkill,
            initial_rating: 1500.0,
            initial_deviation: 250.0,
            min_deviation: 20.0,
            max_deviation: 350.0,
            beta: 125.0,
            trueskill_tau: 2.5,
            draw_probability: 0.1,
            ..Default::default()
        }
    }

    fn player(id: i64, team: usize, rating: f64, deviation: f64) -> PlayerRatingState {
        PlayerRatingState {
            user: UserId(id),
            team,
            rating,
            deviation,
            volatility: 0.06,
            streak: 0,
        }
    }

    #[test]
    fn the_winner_gains_and_the_loser_loses() {
        let roster = vec![player(1, 0, 1500.0, 250.0), player(2, 1, 1500.0, 250.0)];
        let deltas = TrueSkill
            .rate(&roster, MatchOutcome::Winner(0), &config())
            .unwrap();
        assert!(deltas[0].change() > 0.0);
        assert!(deltas[1].change() < 0.0);
        assert!(
            (deltas[0].change() + deltas[1].change()).abs() < 1e-6,
            "an even match should be symmetric"
        );
    }

    #[test]
    fn uncertainty_shrinks_after_every_match() {
        let roster = vec![player(1, 0, 1500.0, 250.0), player(2, 1, 1500.0, 250.0)];
        for outcome in [MatchOutcome::Winner(0), MatchOutcome::Draw] {
            let deltas = TrueSkill.rate(&roster, outcome, &config()).unwrap();
            for delta in deltas {
                assert!(
                    delta.deviation_after < delta.deviation_before,
                    "sigma must contract after evidence"
                );
            }
        }
    }

    #[test]
    fn an_upset_moves_ratings_more_than_an_expected_result() {
        let expected = TrueSkill
            .rate(
                &[player(1, 0, 2000.0, 100.0), player(2, 1, 1000.0, 100.0)],
                MatchOutcome::Winner(0),
                &config(),
            )
            .unwrap()[0]
            .change();
        let upset = TrueSkill
            .rate(
                &[player(1, 0, 1000.0, 100.0), player(2, 1, 2000.0, 100.0)],
                MatchOutcome::Winner(0),
                &config(),
            )
            .unwrap()[0]
            .change();
        assert!(upset > expected, "upset {upset} should exceed {expected}");
        assert!(expected > 0.0, "a favourite still gains something");
    }

    #[test]
    fn a_draw_pulls_the_favourite_down_and_the_underdog_up() {
        let deltas = TrueSkill
            .rate(
                &[player(1, 0, 2000.0, 200.0), player(2, 1, 1200.0, 200.0)],
                MatchOutcome::Draw,
                &config(),
            )
            .unwrap();
        assert!(deltas[0].change() < 0.0, "the favourite underperformed");
        assert!(deltas[1].change() > 0.0, "the underdog overperformed");
    }

    #[test]
    fn a_settled_player_moves_less_than_a_new_one() {
        let new = TrueSkill
            .rate(
                &[player(1, 0, 1500.0, 250.0), player(2, 1, 1500.0, 250.0)],
                MatchOutcome::Winner(0),
                &config(),
            )
            .unwrap()[0]
            .change();
        let settled = TrueSkill
            .rate(
                &[player(1, 0, 1500.0, 25.0), player(2, 1, 1500.0, 250.0)],
                MatchOutcome::Winner(0),
                &config(),
            )
            .unwrap()[0]
            .change();
        assert!(new > settled, "{new} should exceed {settled}");
    }

    #[test]
    fn five_versus_five_rates_every_player() {
        let mut roster = Vec::new();
        for i in 0..10 {
            roster.push(player(
                i + 1,
                (i % 2) as usize,
                1400.0 + i as f64 * 25.0,
                200.0,
            ));
        }
        let deltas = TrueSkill
            .rate(&roster, MatchOutcome::Winner(1), &config())
            .unwrap();
        assert_eq!(deltas.len(), 10);
        for delta in &deltas {
            let team = roster.iter().find(|p| p.user == delta.user).unwrap().team;
            if team == 1 {
                assert!(delta.change() > 0.0);
            } else {
                assert!(delta.change() < 0.0);
            }
        }
    }

    #[test]
    fn a_zero_draw_probability_removes_the_draw_margin() {
        assert_eq!(draw_margin(0.0, 10.0, 125.0), 0.0);
        assert!(draw_margin(0.1, 10.0, 125.0) > 0.0);
        assert!(draw_margin(0.5, 10.0, 125.0) > draw_margin(0.1, 10.0, 125.0));
    }

    #[test]
    fn conservative_rating_is_three_sigma_below_the_mean() {
        assert_eq!(conservative_rating(1500.0, 100.0), 1200.0);
    }

    #[test]
    fn deviation_never_leaves_the_configured_bounds() {
        let mut config = config();
        config.min_deviation = 200.0;
        let roster = vec![player(1, 0, 1500.0, 250.0), player(2, 1, 1500.0, 250.0)];
        let deltas = TrueSkill
            .rate(&roster, MatchOutcome::Winner(0), &config)
            .unwrap();
        for delta in deltas {
            assert!(delta.deviation_after >= 200.0);
            assert!(delta.deviation_after <= config.max_deviation);
        }
    }
}
