//! Glicko-2 adapter.
//!
//! Glicko-2 rates one-on-one games, so each player is rated against a single
//! aggregate opponent formed from the other team: mean rating, and deviations
//! combined in quadrature. This is the usual team adaptation and keeps the
//! per-player update independent of roster order.
//!
//! The configured `win_scale`, `loss_scale`, `draw_bonus` and streak
//! multipliers are applied to the resulting rating change. They default to
//! neutral values, so an untouched configuration is textbook Glicko-2.

use std::f64::consts::PI;

use super::{
    opponent_aggregate, streak_multiplier, validate_roster, MatchOutcome, PlayerRatingState,
    RatingConfig, RatingDelta, RatingSystem, RatingSystemKind,
};
use crate::error::DomainResult;

/// Glicko-2's internal scale factor, 400 / ln(10).
const SCALE: f64 = 173.717_792_761_770_5;
/// Convergence tolerance for the volatility iteration.
const EPSILON: f64 = 1e-6;
/// Safety bound on the volatility bracketing search.
const MAX_ITERATIONS: usize = 100;

/// The Glicko-2 rating adapter. See the [module documentation](self).
#[derive(Debug, Clone, Copy, Default)]
pub struct Glicko2;

impl RatingSystem for Glicko2 {
    fn kind(&self) -> RatingSystemKind {
        RatingSystemKind::Glicko2
    }

    fn rate(
        &self,
        players: &[PlayerRatingState],
        outcome: MatchOutcome,
        config: &RatingConfig,
    ) -> DomainResult<Vec<RatingDelta>> {
        validate_roster(players, outcome)?;

        let center = config.initial_rating;
        let deltas = players
            .iter()
            .map(|player| {
                let (opponent_rating, opponent_deviation) =
                    opponent_aggregate(players, player.team);
                let score = outcome.score_for(player.team);

                // Step 2: to the Glicko-2 scale.
                let mu = (player.rating - center) / SCALE;
                let phi = (player.deviation / SCALE).max(1e-9);
                let mu_j = (opponent_rating - center) / SCALE;
                let phi_j = (opponent_deviation / SCALE).max(1e-9);

                // Step 3-4: estimated variance and improvement.
                let g = g(phi_j);
                let expected = expected_score(mu, mu_j, phi_j);
                let variance = 1.0 / (g * g * expected * (1.0 - expected)).max(1e-12);
                let improvement = variance * g * (score - expected);

                // Step 5: new volatility.
                let volatility = new_volatility(
                    player.volatility.max(1e-6),
                    phi,
                    variance,
                    improvement,
                    config.tau,
                );

                // Step 6-7: pre-rating-period deviation, then the update.
                let phi_star = (phi * phi + volatility * volatility).sqrt();
                let phi_new = 1.0 / (1.0 / (phi_star * phi_star) + 1.0 / variance).sqrt();
                let mu_new = mu + phi_new * phi_new * g * (score - expected);

                // Step 8: back to the display scale.
                let raw_rating = center + SCALE * mu_new;
                let raw_change = raw_rating - player.rating;

                let adjusted_change = match outcome {
                    MatchOutcome::Draw => raw_change + config.draw_bonus,
                    MatchOutcome::Winner(winner) if winner == player.team => {
                        raw_change
                            * config.win_scale
                            * streak_multiplier(config, player.streak, true)
                    }
                    MatchOutcome::Winner(_) => {
                        raw_change
                            * config.loss_scale
                            * streak_multiplier(config, player.streak, false)
                    }
                };

                let deviation_after =
                    (SCALE * phi_new).clamp(config.min_deviation, config.max_deviation);

                RatingDelta {
                    user: player.user,
                    rating_before: player.rating,
                    rating_after: player.rating + adjusted_change,
                    deviation_before: player.deviation,
                    deviation_after,
                    volatility_after: volatility,
                }
            })
            .collect();
        Ok(deltas)
    }
}

fn g(phi: f64) -> f64 {
    1.0 / (1.0 + 3.0 * phi * phi / (PI * PI)).sqrt()
}

fn expected_score(mu: f64, mu_j: f64, phi_j: f64) -> f64 {
    1.0 / (1.0 + (-g(phi_j) * (mu - mu_j)).exp())
}

/// Step 5 of Glicko-2: solve for the new volatility with the Illinois variant
/// of regula falsi, exactly as specified by Glickman.
fn new_volatility(sigma: f64, phi: f64, variance: f64, improvement: f64, tau: f64) -> f64 {
    let a = (sigma * sigma).ln();
    let delta_squared = improvement * improvement;
    let phi_squared = phi * phi;

    let f = |x: f64| {
        let exp_x = x.exp();
        let numerator = exp_x * (delta_squared - phi_squared - variance - exp_x);
        let denominator = 2.0 * (phi_squared + variance + exp_x).powi(2);
        numerator / denominator - (x - a) / (tau * tau)
    };

    let mut lower = a;
    let mut upper = if delta_squared > phi_squared + variance {
        (delta_squared - phi_squared - variance).ln()
    } else {
        let mut k = 1.0;
        while f(a - k * tau) < 0.0 && k < MAX_ITERATIONS as f64 {
            k += 1.0;
        }
        a - k * tau
    };

    let mut f_lower = f(lower);
    let mut f_upper = f(upper);
    let mut iterations = 0;
    while (upper - lower).abs() > EPSILON && iterations < MAX_ITERATIONS {
        let mid = lower + (lower - upper) * f_lower / (f_upper - f_lower);
        let f_mid = f(mid);
        if f_mid * f_upper <= 0.0 {
            lower = upper;
            f_lower = f_upper;
        } else {
            f_lower /= 2.0;
        }
        upper = mid;
        f_upper = f_mid;
        iterations += 1;
    }
    (lower / 2.0).exp()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::ids::UserId;

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

    /// Glickman's own worked example: a 1500/200/0.06 player who beats a
    /// 1400/30 opponent, loses to 1550/100 and 1700/300, ends at roughly
    /// 1464.06 with deviation 151.52. Reproducing it one game at a time is not
    /// possible, so this checks the single-opponent case behaves sanely and
    /// the internal helpers match the published intermediate values.
    #[test]
    fn helper_functions_match_the_published_example() {
        assert!((g(30.0 / SCALE) - 0.9955).abs() < 1e-4);
        assert!((g(100.0 / SCALE) - 0.9531).abs() < 1e-4);
        assert!((g(300.0 / SCALE) - 0.7242).abs() < 1e-4);

        let mu = 0.0;
        assert!((expected_score(mu, (1400.0 - 1500.0) / SCALE, 30.0 / SCALE) - 0.639).abs() < 1e-3);
        assert!(
            (expected_score(mu, (1550.0 - 1500.0) / SCALE, 100.0 / SCALE) - 0.432).abs() < 1e-3
        );
        assert!(
            (expected_score(mu, (1700.0 - 1500.0) / SCALE, 300.0 / SCALE) - 0.303).abs() < 1e-3
        );
    }

    #[test]
    fn beating_a_stronger_opponent_gains_more_than_beating_a_weaker_one() {
        let config = RatingConfig::default();
        let over_underdog = Glicko2
            .rate(
                &[player(1, 0, 1500.0, 200.0), player(2, 1, 1200.0, 200.0)],
                MatchOutcome::Winner(0),
                &config,
            )
            .unwrap()[0]
            .change();
        let over_favourite = Glicko2
            .rate(
                &[player(1, 0, 1500.0, 200.0), player(2, 1, 1800.0, 200.0)],
                MatchOutcome::Winner(0),
                &config,
            )
            .unwrap()[0]
            .change();
        assert!(
            over_favourite > over_underdog,
            "{over_favourite} should exceed {over_underdog}"
        );
    }

    #[test]
    fn an_uncertain_player_moves_further_than_a_settled_one() {
        let config = RatingConfig::default();
        let uncertain = Glicko2
            .rate(
                &[player(1, 0, 1500.0, 350.0), player(2, 1, 1500.0, 50.0)],
                MatchOutcome::Winner(0),
                &config,
            )
            .unwrap()[0]
            .change();
        let settled = Glicko2
            .rate(
                &[player(1, 0, 1500.0, 50.0), player(2, 1, 1500.0, 50.0)],
                MatchOutcome::Winner(0),
                &config,
            )
            .unwrap()[0]
            .change();
        assert!(uncertain > settled, "{uncertain} should exceed {settled}");
    }

    #[test]
    fn deviation_shrinks_after_a_rated_match() {
        let config = RatingConfig::default();
        let deltas = Glicko2
            .rate(
                &[player(1, 0, 1500.0, 300.0), player(2, 1, 1500.0, 300.0)],
                MatchOutcome::Winner(0),
                &config,
            )
            .unwrap();
        for delta in deltas {
            assert!(delta.deviation_after < delta.deviation_before);
            assert!(delta.deviation_after >= config.min_deviation);
        }
    }

    #[test]
    fn an_even_draw_barely_moves_either_player() {
        let config = RatingConfig::default();
        let deltas = Glicko2
            .rate(
                &[player(1, 0, 1500.0, 100.0), player(2, 1, 1500.0, 100.0)],
                MatchOutcome::Draw,
                &config,
            )
            .unwrap();
        assert!(deltas[0].change().abs() < 1e-6);
        assert!(deltas[1].change().abs() < 1e-6);
    }

    #[test]
    fn volatility_stays_positive_and_finite_across_extremes() {
        let config = RatingConfig::default();
        for (a, b) in [(100.0, 3000.0), (3000.0, 100.0), (1500.0, 1500.0)] {
            let deltas = Glicko2
                .rate(
                    &[player(1, 0, a, 350.0), player(2, 1, b, 350.0)],
                    MatchOutcome::Winner(0),
                    &config,
                )
                .unwrap();
            for delta in deltas {
                assert!(delta.volatility_after > 0.0 && delta.volatility_after < 10.0);
                assert!(delta.rating_after.is_finite());
            }
        }
    }

    #[test]
    fn team_ratings_use_the_whole_opposing_side() {
        let config = RatingConfig::default();
        let roster = vec![
            player(1, 0, 1500.0, 100.0),
            player(2, 0, 1500.0, 100.0),
            player(3, 1, 1000.0, 100.0),
            player(4, 1, 2000.0, 100.0),
        ];
        let deltas = Glicko2
            .rate(&roster, MatchOutcome::Winner(0), &config)
            .unwrap();
        // Both winners face the same 1500-average opposition, so they move
        // identically regardless of position in the roster.
        assert!((deltas[0].change() - deltas[1].change()).abs() < 1e-9);
    }
}
