//! Captain selection and team construction.
//!
//! All randomness is injected as `&mut impl Rng` so tests can seed it and
//! assert on exact outcomes.

use rand::seq::SliceRandom;
use rand::Rng;

use crate::domain::ids::UserId;
use crate::domain::settings::CaptainMode;
use crate::error::{DomainError, DomainResult};

/// A player as team formation sees them: an id, a rating, and whether they
/// hold the queue's captain role.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlayerSeed {
    /// Who the player is.
    pub user: UserId,
    /// Their current rating in the channel's rating pool.
    pub rating: f64,
    /// Whether they hold the queue's configured captain role.
    pub has_captain_role: bool,
}

impl PlayerSeed {
    /// A seed for a player who does not hold the captain role.
    #[must_use]
    pub fn new(user: UserId, rating: f64) -> Self {
        Self {
            user,
            rating,
            has_captain_role: false,
        }
    }
}

/// Above this roster size the exhaustive balanced split is replaced by a
/// greedy one. 2^20 masks is a few milliseconds; beyond that it is not.
const EXHAUSTIVE_LIMIT: usize = 20;

/// Chooses `team_count` captains according to `mode`.
///
/// Returns an empty vector for [`CaptainMode::Volunteer`], where players claim
/// slots themselves with `/capfor`.
///
/// # Errors
///
/// Returns [`DomainError::InvalidConfig`]
/// if there are fewer players than captains required.
pub fn select_captains<R: Rng + ?Sized>(
    players: &[PlayerSeed],
    mode: CaptainMode,
    team_count: usize,
    rng: &mut R,
) -> DomainResult<Vec<UserId>> {
    if mode == CaptainMode::Volunteer {
        return Ok(Vec::new());
    }
    if players.len() < team_count {
        return Err(DomainError::InvalidConfig(format!(
            "need at least {team_count} players to choose {team_count} captains"
        )));
    }

    let chosen = match mode {
        CaptainMode::Volunteer => unreachable!("handled above"),
        CaptainMode::RoleAndRating => {
            let mut ranked = players.to_vec();
            ranked.sort_by(|a, b| {
                b.has_captain_role
                    .cmp(&a.has_captain_role)
                    .then(b.rating.total_cmp(&a.rating))
                    .then(a.user.cmp(&b.user))
            });
            ranked[..team_count].iter().map(|p| p.user).collect()
        }
        CaptainMode::FairPair => {
            // The tightest window of `team_count` players once sorted by
            // rating minimises the spread between captains.
            let mut ranked = players.to_vec();
            ranked.sort_by(|a, b| a.rating.total_cmp(&b.rating).then(a.user.cmp(&b.user)));
            let mut best_start = 0;
            let mut best_spread = f64::INFINITY;
            for start in 0..=(ranked.len() - team_count) {
                let spread = ranked[start + team_count - 1].rating - ranked[start].rating;
                if spread < best_spread {
                    best_spread = spread;
                    best_start = start;
                }
            }
            ranked[best_start..best_start + team_count]
                .iter()
                .map(|p| p.user)
                .collect()
        }
        CaptainMode::RandomWithRolePreference => {
            let mut with_role: Vec<UserId> = players
                .iter()
                .filter(|p| p.has_captain_role)
                .map(|p| p.user)
                .collect();
            let mut without_role: Vec<UserId> = players
                .iter()
                .filter(|p| !p.has_captain_role)
                .map(|p| p.user)
                .collect();
            with_role.shuffle(rng);
            without_role.shuffle(rng);
            with_role.extend(without_role);
            with_role.truncate(team_count);
            with_role
        }
        CaptainMode::Random => {
            let mut all: Vec<UserId> = players.iter().map(|p| p.user).collect();
            all.shuffle(rng);
            all.truncate(team_count);
            all
        }
    };
    Ok(chosen)
}

/// Splits players into equal teams, minimising the rating spread.
///
/// For two teams and a roster of at most 20 players this searches every
/// balanced split and is therefore optimal. Larger rosters, and any roster
/// split into more than two teams, fall back to a greedy assignment refined by
/// pairwise swaps — good, but not provably optimal.
///
/// # Errors
///
/// Returns [`DomainError::InvalidConfig`] if the roster size does not match
/// `team_count * team_size`, if either is zero, or if a player appears twice.
pub fn balanced_teams(
    players: &[PlayerSeed],
    team_count: usize,
    team_size: usize,
) -> DomainResult<Vec<Vec<UserId>>> {
    validate_shape(players, team_count, team_size)?;

    if team_count == 2 && players.len() <= EXHAUSTIVE_LIMIT {
        return Ok(optimal_two_way_split(players, team_size));
    }
    Ok(greedy_snake(players, team_count, team_size))
}

fn optimal_two_way_split(players: &[PlayerSeed], team_size: usize) -> Vec<Vec<UserId>> {
    let n = players.len();
    let total: f64 = players.iter().map(|p| p.rating).sum();
    let mut best_mask = 0u32;
    let mut best_diff = f64::INFINITY;

    // Fix player 0 on team A to skip the mirror image of every split.
    for mask in 0u32..(1u32 << n) {
        if mask & 1 == 0 || mask.count_ones() as usize != team_size {
            continue;
        }
        let sum_a: f64 = (0..n)
            .filter(|&i| mask & (1 << i) != 0)
            .map(|i| players[i].rating)
            .sum();
        let diff = (2.0 * sum_a - total).abs();
        if diff < best_diff {
            best_diff = diff;
            best_mask = mask;
        }
    }

    let mut teams = vec![Vec::with_capacity(team_size), Vec::with_capacity(team_size)];
    for (index, player) in players.iter().enumerate() {
        let side = usize::from(best_mask & (1 << index) == 0);
        teams[side].push(player.user);
    }
    teams
}

/// Maximum swap-refinement sweeps. Each sweep is O(n²); convergence is
/// typically reached in two or three.
const MAX_REFINEMENT_SWEEPS: usize = 32;

fn greedy_snake(players: &[PlayerSeed], team_count: usize, team_size: usize) -> Vec<Vec<UserId>> {
    let mut ranked = players.to_vec();
    ranked.sort_by(|a, b| b.rating.total_cmp(&a.rating).then(a.user.cmp(&b.user)));

    let mut teams: Vec<Vec<PlayerSeed>> = vec![Vec::with_capacity(team_size); team_count];
    let mut totals = vec![0.0f64; team_count];
    for player in ranked {
        // Weakest team with room takes the next strongest player.
        let target = (0..team_count)
            .filter(|&i| teams[i].len() < team_size)
            .min_by(|&a, &b| totals[a].total_cmp(&totals[b]).then(a.cmp(&b)))
            .expect("some team always has room while players remain");
        teams[target].push(player);
        totals[target] += player.rating;
    }

    refine_by_swapping(&mut teams, &mut totals);

    teams
        .into_iter()
        .map(|team| team.into_iter().map(|p| p.user).collect())
        .collect()
}

/// Greedy assignment alone leaves obvious improvements on the table, because it
/// cannot undo an early choice. This hill-climbs by swapping one player between
/// two teams whenever that reduces the total squared deviation from the mean.
///
/// Squared deviation rather than max-minus-min: the latter is flat across many
/// swaps (moving weight between two teams that are not the extremes does not
/// change it), so it stalls immediately. This is still a local search — it can
/// stop at a local optimum that a two-swap sequence would escape — so
/// three-or-more-team balance is good, not provably optimal. Two-team balance
/// does not use this path at all; it is solved exactly above.
fn refine_by_swapping(teams: &mut [Vec<PlayerSeed>], totals: &mut [f64]) {
    let cost = |totals: &[f64]| {
        let mean = totals.iter().sum::<f64>() / totals.len() as f64;
        totals.iter().map(|t| (t - mean).powi(2)).sum::<f64>()
    };

    for _ in 0..MAX_REFINEMENT_SWEEPS {
        let mut improved = false;
        let current = cost(totals);
        'sweep: for a in 0..teams.len() {
            for b in (a + 1)..teams.len() {
                for i in 0..teams[a].len() {
                    for j in 0..teams[b].len() {
                        let shift = teams[b][j].rating - teams[a][i].rating;
                        if shift == 0.0 {
                            continue;
                        }
                        let mut candidate = totals.to_vec();
                        candidate[a] += shift;
                        candidate[b] -= shift;
                        if cost(&candidate) < current - 1e-9 {
                            let from_a = teams[a][i];
                            let from_b = teams[b][j];
                            teams[a][i] = from_b;
                            teams[b][j] = from_a;
                            totals.copy_from_slice(&candidate);
                            improved = true;
                            break 'sweep;
                        }
                    }
                }
            }
        }
        if !improved {
            break;
        }
    }
}

/// Shuffles players into equal teams, ignoring ratings.
///
/// # Errors
///
/// Returns [`DomainError::InvalidConfig`]
/// if the roster size does not match `team_count * team_size`, if either is
/// zero, or if a player appears twice.
pub fn random_teams<R: Rng + ?Sized>(
    players: &[PlayerSeed],
    team_count: usize,
    team_size: usize,
    rng: &mut R,
) -> DomainResult<Vec<Vec<UserId>>> {
    validate_shape(players, team_count, team_size)?;
    let mut shuffled: Vec<UserId> = players.iter().map(|p| p.user).collect();
    shuffled.shuffle(rng);
    Ok(shuffled
        .chunks(team_size)
        .map(<[UserId]>::to_vec)
        .collect::<Vec<_>>())
}

fn validate_shape(players: &[PlayerSeed], team_count: usize, team_size: usize) -> DomainResult<()> {
    if team_count == 0 || team_size == 0 {
        return Err(DomainError::InvalidConfig(
            "team count and team size must both be positive".into(),
        ));
    }
    if players.len() != team_count * team_size {
        return Err(DomainError::InvalidConfig(format!(
            "expected {} players for {team_count}x{team_size}, got {}",
            team_count * team_size,
            players.len()
        )));
    }
    let mut ids: Vec<UserId> = players.iter().map(|p| p.user).collect();
    ids.sort_unstable();
    ids.dedup();
    if ids.len() != players.len() {
        return Err(DomainError::InvalidConfig(
            "the same player appears twice in the roster".into(),
        ));
    }
    Ok(())
}

/// Sum of a team's ratings, for display in the teams embed.
pub fn team_rating(team: &[UserId], players: &[PlayerSeed]) -> f64 {
    team.iter()
        .filter_map(|user| players.iter().find(|p| p.user == *user))
        .map(|p| p.rating)
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::StdRng;
    use rand::SeedableRng;

    fn seeds(ratings: &[f64]) -> Vec<PlayerSeed> {
        ratings
            .iter()
            .enumerate()
            .map(|(i, &r)| PlayerSeed::new(UserId(i as i64 + 1), r))
            .collect()
    }

    fn sum(team: &[UserId], players: &[PlayerSeed]) -> f64 {
        team_rating(team, players)
    }

    #[test]
    fn balanced_split_is_optimal_for_two_teams() {
        let players = seeds(&[1000.0, 1200.0, 1400.0, 1600.0, 1800.0, 2000.0]);
        let teams = balanced_teams(&players, 2, 3).unwrap();
        let diff = (sum(&teams[0], &players) - sum(&teams[1], &players)).abs();
        // Total is 9000, so a perfect split would be 4500 each. No three of
        // these values sum to 4500; the best achievable is 4600/4400.
        assert_eq!(diff, 200.0);
    }

    #[test]
    fn a_perfectly_balanced_roster_splits_exactly() {
        let players = seeds(&[1000.0, 1500.0, 2000.0, 1000.0, 1500.0, 2000.0]);
        let teams = balanced_teams(&players, 2, 3).unwrap();
        let diff = (sum(&teams[0], &players) - sum(&teams[1], &players)).abs();
        assert_eq!(diff, 0.0);
    }

    #[test]
    fn balanced_split_handles_an_unbalanceable_roster() {
        let players = seeds(&[100.0, 100.0, 100.0, 5000.0]);
        let teams = balanced_teams(&players, 2, 2).unwrap();
        let diff = (sum(&teams[0], &players) - sum(&teams[1], &players)).abs();
        assert_eq!(diff, 4900.0, "the best possible split pairs 5000 with 100");
    }

    #[test]
    fn balanced_split_partitions_every_player_exactly_once() {
        let players = seeds(&[1500.0, 900.0, 1100.0, 1750.0, 1300.0, 1450.0, 800.0, 2100.0]);
        let teams = balanced_teams(&players, 2, 4).unwrap();
        assert_eq!(teams.len(), 2);
        assert!(teams.iter().all(|t| t.len() == 4));
        let mut all: Vec<UserId> = teams.concat();
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), 8);
    }

    #[test]
    fn greedy_split_covers_more_than_two_teams() {
        let players = seeds(&[
            100.0, 200.0, 300.0, 400.0, 500.0, 600.0, 700.0, 800.0, 900.0,
        ]);
        let teams = balanced_teams(&players, 3, 3).unwrap();
        assert_eq!(teams.len(), 3);
        let totals: Vec<f64> = teams.iter().map(|t| sum(t, &players)).collect();
        let spread = totals.iter().cloned().fold(f64::MIN, f64::max)
            - totals.iter().cloned().fold(f64::MAX, f64::min);
        // A perfect 1500/1500/1500 split exists, but reaching it from the
        // greedy start needs two swaps and local search only takes strictly
        // improving single swaps. Three-plus-team balance is best-effort; the
        // guarantee is that no team is badly adrift, not optimality.
        assert!(spread <= 200.0, "unbalanced split: {totals:?}");
    }

    #[test]
    fn swap_refinement_never_breaks_the_partition() {
        let players = seeds(&[
            1523.0, 998.0, 1740.0, 1201.0, 1655.0, 890.0, 2010.0, 1333.0, 1444.0, 1120.0, 1890.0,
            1077.0,
        ]);
        let teams = balanced_teams(&players, 3, 4).unwrap();
        assert_eq!(teams.len(), 3);
        assert!(teams.iter().all(|t| t.len() == 4));
        let mut all: Vec<UserId> = teams.concat();
        all.sort_unstable();
        assert_eq!(all, (1..=12).map(UserId).collect::<Vec<_>>());
    }

    #[test]
    fn wrong_roster_size_is_rejected() {
        let players = seeds(&[1.0, 2.0, 3.0]);
        assert!(balanced_teams(&players, 2, 2).is_err());
        let mut rng = StdRng::seed_from_u64(1);
        assert!(random_teams(&players, 2, 2, &mut rng).is_err());
    }

    #[test]
    fn random_teams_partition_the_roster() {
        let players = seeds(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0]);
        let mut rng = StdRng::seed_from_u64(42);
        let teams = random_teams(&players, 2, 4, &mut rng).unwrap();
        let mut all: Vec<UserId> = teams.concat();
        all.sort_unstable();
        assert_eq!(all, (1..=8).map(UserId).collect::<Vec<_>>());
    }

    #[test]
    fn role_and_rating_prefers_captain_role_holders_over_higher_ratings() {
        let mut players = seeds(&[2500.0, 2400.0, 1000.0, 900.0]);
        players[2].has_captain_role = true;
        let mut rng = StdRng::seed_from_u64(7);
        let captains = select_captains(&players, CaptainMode::RoleAndRating, 2, &mut rng).unwrap();
        assert_eq!(captains[0], UserId(3), "the role holder outranks everyone");
        assert_eq!(captains[1], UserId(1), "then the highest rating");
    }

    #[test]
    fn fair_pair_picks_the_closest_ratings() {
        let players = seeds(&[1000.0, 1990.0, 2000.0, 3000.0]);
        let mut rng = StdRng::seed_from_u64(7);
        let captains = select_captains(&players, CaptainMode::FairPair, 2, &mut rng).unwrap();
        let mut ids = captains;
        ids.sort_unstable();
        assert_eq!(ids, vec![UserId(2), UserId(3)]);
    }

    #[test]
    fn random_with_role_preference_never_skips_a_role_holder() {
        let mut players = seeds(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);
        players[4].has_captain_role = true;
        players[5].has_captain_role = true;
        for seed in 0..25u64 {
            let mut rng = StdRng::seed_from_u64(seed);
            let captains =
                select_captains(&players, CaptainMode::RandomWithRolePreference, 2, &mut rng)
                    .unwrap();
            let mut ids = captains;
            ids.sort_unstable();
            assert_eq!(ids, vec![UserId(5), UserId(6)]);
        }
    }

    #[test]
    fn volunteer_mode_appoints_nobody() {
        let players = seeds(&[1.0, 2.0, 3.0, 4.0]);
        let mut rng = StdRng::seed_from_u64(1);
        let captains = select_captains(&players, CaptainMode::Volunteer, 2, &mut rng).unwrap();
        assert!(captains.is_empty());
    }

    #[test]
    fn captain_selection_never_returns_duplicates() {
        let players = seeds(&[1000.0, 1000.0, 1000.0, 1000.0, 1000.0, 1000.0]);
        for mode in [
            CaptainMode::RoleAndRating,
            CaptainMode::FairPair,
            CaptainMode::RandomWithRolePreference,
            CaptainMode::Random,
        ] {
            for seed in 0..20u64 {
                let mut rng = StdRng::seed_from_u64(seed);
                let captains = select_captains(&players, mode, 2, &mut rng).unwrap();
                assert_eq!(captains.len(), 2);
                assert_ne!(captains[0], captains[1], "{mode:?} returned a duplicate");
            }
        }
    }
}
