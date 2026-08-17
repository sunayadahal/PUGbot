//! Map pool selection, recent-map avoidance, and map voting.

use std::collections::BTreeMap;

use rand::seq::SliceRandom;
use rand::Rng;
use serde::{Deserialize, Serialize};

use crate::domain::ids::UserId;
use crate::domain::settings::TieBreak;
use crate::error::{DomainError, DomainResult};

/// Maps played recently, most recent first. Entries may repeat.
pub type RecentMaps<'a> = &'a [String];

/// Returns the subset of `pool` that is not on cooldown.
///
/// The cooldown is relaxed one match at a time when honouring it in full would
/// leave fewer than `needed` maps available, so a small pool never deadlocks.
pub fn available_maps(
    pool: &[String],
    recent: RecentMaps<'_>,
    cooldown_matches: usize,
    needed: usize,
) -> Vec<String> {
    let mut cooldown = cooldown_matches.min(recent.len());
    loop {
        let blocked = &recent[..cooldown];
        let available: Vec<String> = pool
            .iter()
            .filter(|map| !blocked.iter().any(|b| b.eq_ignore_ascii_case(map)))
            .cloned()
            .collect();
        if available.len() >= needed || cooldown == 0 {
            return if available.is_empty() {
                pool.to_vec()
            } else {
                available
            };
        }
        cooldown -= 1;
    }
}

/// Picks `count` distinct maps at random, avoiding recently played ones.
///
/// # Errors
///
/// Returns [`DomainError::EmptyMapPool`] if the pool is empty, or
/// [`DomainError::InvalidConfig`] if `count` exceeds the pool size.
pub fn select_maps<R: Rng + ?Sized>(
    pool: &[String],
    count: usize,
    recent: RecentMaps<'_>,
    cooldown_matches: usize,
    rng: &mut R,
) -> DomainResult<Vec<String>> {
    if pool.is_empty() {
        return Err(DomainError::EmptyMapPool);
    }
    if count == 0 {
        return Ok(Vec::new());
    }
    if count > pool.len() {
        return Err(DomainError::InvalidConfig(format!(
            "cannot pick {count} maps from a pool of {}",
            pool.len()
        )));
    }
    let mut available = available_maps(pool, recent, cooldown_matches, count);
    available.shuffle(rng);
    available.truncate(count);
    Ok(available)
}

/// An in-progress map vote. Candidates are fixed when the vote opens; each
/// eligible user holds at most one vote, and recasting replaces it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MapVote {
    /// The maps put to the vote, in the order they are displayed. Indices into
    /// this vector are what a ballot records.
    pub candidates: Vec<String>,
    /// user -> candidate index.
    pub votes: BTreeMap<UserId, usize>,
    /// Users allowed to vote; empty means anybody in the match.
    pub eligible: Vec<UserId>,
}

impl MapVote {
    /// Opens a vote over `candidates`, restricted to `eligible` voters.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::InvalidVoteSize`] unless there are between 2 and
    /// 9 candidates. The upper bound keeps the ballot inside Discord's limit of
    /// five buttons per action row across two rows, and keeps it readable.
    pub fn new(candidates: Vec<String>, eligible: Vec<UserId>) -> DomainResult<Self> {
        if !(2..=9).contains(&candidates.len()) {
            return Err(DomainError::InvalidVoteSize(candidates.len()));
        }
        Ok(Self {
            candidates,
            votes: BTreeMap::new(),
            eligible,
        })
    }

    /// Records a ballot, replacing any previous one from the same user.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::InvalidConfig`] if `candidate` is out of range,
    /// or [`DomainError::NotInMatch`] if the user may not vote here.
    pub fn cast(&mut self, user: UserId, candidate: usize) -> DomainResult<()> {
        if candidate >= self.candidates.len() {
            return Err(DomainError::InvalidConfig(format!(
                "no map candidate {candidate}"
            )));
        }
        if !self.eligible.is_empty() && !self.eligible.contains(&user) {
            return Err(DomainError::NotInMatch);
        }
        self.votes.insert(user, candidate);
        Ok(())
    }

    /// Vote count per candidate, indexed the same way as `candidates`.
    pub fn tally(&self) -> Vec<usize> {
        let mut counts = vec![0usize; self.candidates.len()];
        for &candidate in self.votes.values() {
            counts[candidate] += 1;
        }
        counts
    }

    /// Whether every eligible voter has cast a ballot, which lets the vote
    /// close early instead of waiting for its deadline.
    #[must_use]
    pub fn everyone_voted(&self) -> bool {
        !self.eligible.is_empty() && self.votes.len() >= self.eligible.len()
    }

    /// Resolves the vote into `count` winning maps, highest first.
    ///
    /// Ties are broken by candidate order or randomly, per `tie_break`. With no
    /// votes at all this degrades to the configured tie-break over all
    /// candidates, so a silent lobby still gets a map.
    pub fn resolve<R: Rng + ?Sized>(
        &self,
        count: usize,
        tie_break: TieBreak,
        rng: &mut R,
    ) -> Vec<String> {
        let counts = self.tally();
        let mut indices: Vec<usize> = (0..self.candidates.len()).collect();
        if tie_break == TieBreak::Random {
            indices.shuffle(rng);
        }
        // A stable sort over an order that is either original or pre-shuffled
        // gives exactly the requested tie-break behaviour.
        indices.sort_by(|&a, &b| counts[b].cmp(&counts[a]));
        indices
            .into_iter()
            .take(count.min(self.candidates.len()))
            .map(|index| self.candidates[index].clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::StdRng;
    use rand::SeedableRng;

    fn maps(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn recent_maps_are_avoided_when_the_pool_allows() {
        let pool = maps(&["a", "b", "c", "d", "e"]);
        let recent = maps(&["a", "b"]);
        let available = available_maps(&pool, &recent, 2, 1);
        assert_eq!(available, maps(&["c", "d", "e"]));
    }

    #[test]
    fn cooldown_relaxes_rather_than_starving_a_small_pool() {
        let pool = maps(&["a", "b", "c"]);
        let recent = maps(&["a", "b", "c"]);
        // Honouring all three would leave nothing; the oldest entry is released
        // until enough maps are available.
        let available = available_maps(&pool, &recent, 3, 2);
        assert_eq!(available.len(), 2);
        assert!(
            !available.contains(&"a".to_string()),
            "newest stays blocked"
        );
    }

    #[test]
    fn cooldown_matching_is_case_insensitive() {
        let pool = maps(&["Dust2", "Inferno"]);
        let recent = maps(&["dust2"]);
        assert_eq!(available_maps(&pool, &recent, 1, 1), maps(&["Inferno"]));
    }

    #[test]
    fn selection_returns_distinct_maps() {
        let pool = maps(&["a", "b", "c", "d"]);
        let mut rng = StdRng::seed_from_u64(3);
        let picked = select_maps(&pool, 3, &[], 0, &mut rng).unwrap();
        let mut sorted = picked.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), 3);
    }

    #[test]
    fn selection_rejects_an_empty_pool_and_oversized_requests() {
        let mut rng = StdRng::seed_from_u64(3);
        assert_eq!(
            select_maps(&[], 1, &[], 0, &mut rng).unwrap_err(),
            DomainError::EmptyMapPool
        );
        assert!(select_maps(&maps(&["a"]), 2, &[], 0, &mut rng).is_err());
    }

    #[test]
    fn vote_size_is_bounded() {
        assert!(MapVote::new(maps(&["a"]), vec![]).is_err());
        assert!(MapVote::new(maps(&["a", "b"]), vec![]).is_ok());
        let ten: Vec<String> = (0..10).map(|i| format!("m{i}")).collect();
        assert!(MapVote::new(ten, vec![]).is_err());
    }

    #[test]
    fn recasting_replaces_the_previous_vote() {
        let mut vote = MapVote::new(maps(&["a", "b", "c"]), vec![UserId(1)]).unwrap();
        vote.cast(UserId(1), 0).unwrap();
        vote.cast(UserId(1), 2).unwrap();
        assert_eq!(vote.tally(), vec![0, 0, 1]);
    }

    #[test]
    fn only_eligible_players_may_vote() {
        let mut vote = MapVote::new(maps(&["a", "b"]), vec![UserId(1)]).unwrap();
        assert_eq!(
            vote.cast(UserId(2), 0).unwrap_err(),
            DomainError::NotInMatch
        );
        assert!(vote.cast(UserId(1), 5).is_err(), "unknown candidate");
    }

    #[test]
    fn the_most_voted_map_wins() {
        let mut vote = MapVote::new(
            maps(&["a", "b", "c"]),
            vec![UserId(1), UserId(2), UserId(3)],
        )
        .unwrap();
        vote.cast(UserId(1), 1).unwrap();
        vote.cast(UserId(2), 1).unwrap();
        vote.cast(UserId(3), 0).unwrap();
        let mut rng = StdRng::seed_from_u64(1);
        assert_eq!(vote.resolve(1, TieBreak::Random, &mut rng), vec!["b"]);
        assert!(vote.everyone_voted());
    }

    #[test]
    fn deterministic_tie_break_prefers_the_first_candidate() {
        let mut vote = MapVote::new(maps(&["a", "b"]), vec![UserId(1), UserId(2)]).unwrap();
        vote.cast(UserId(1), 0).unwrap();
        vote.cast(UserId(2), 1).unwrap();
        let mut rng = StdRng::seed_from_u64(99);
        assert_eq!(
            vote.resolve(1, TieBreak::Deterministic, &mut rng),
            vec!["a"]
        );
    }

    #[test]
    fn a_vote_with_no_ballots_still_produces_a_map() {
        let vote = MapVote::new(maps(&["a", "b", "c"]), vec![UserId(1)]).unwrap();
        let mut rng = StdRng::seed_from_u64(5);
        let resolved = vote.resolve(1, TieBreak::Random, &mut rng);
        assert_eq!(resolved.len(), 1);
        assert!(vote.candidates.contains(&resolved[0]));
        assert!(!vote.everyone_voted());
    }
}
