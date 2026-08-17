//! Captain draft state machine.
//!
//! The draft is a pure value: services load it from the match row, apply one
//! command, and persist the result together with a bumped version. Nothing here
//! performs I/O, so every pick order can be exercised in unit tests.
//!
//! # Example
//!
//! ```
//! use pugbot::domain::draft::{Draft, PickOrder};
//! use pugbot::domain::ids::UserId;
//!
//! let players: Vec<UserId> = (1..=6).map(UserId).collect();
//! let mut draft = Draft::new(players, 2, 3, PickOrder::parse("ABBA")?)?;
//!
//! draft.set_captain(0, UserId(1))?;
//! draft.set_captain(1, UserId(2))?;
//! assert_eq!(draft.current_captain(), Some(UserId(1)));
//!
//! // Only the captain on the clock may pick.
//! assert!(draft.pick(UserId(2), UserId(3)).is_err());
//! draft.pick(UserId(1), UserId(3))?;
//! assert_eq!(draft.current_captain(), Some(UserId(2)));
//! # Ok::<(), pugbot::error::DomainError>(())
//! ```

use std::fmt;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::domain::ids::UserId;
use crate::error::{DomainError, DomainResult};

/// A repeating pattern of team indices, written as letters: `ABBA` means team A
/// picks, then B twice, then A. The pattern cycles if more picks are needed
/// than it describes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickOrder {
    pattern: Vec<usize>,
}

impl Default for PickOrder {
    fn default() -> Self {
        Self {
            pattern: vec![0, 1],
        }
    }
}

impl PickOrder {
    /// Parses a pattern such as `ABBA`.
    ///
    /// Letters are case-insensitive and map to team indices: `A` is team 0.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::InvalidPickOrder`] if the string is empty or
    /// contains anything other than ASCII letters.
    pub fn parse(raw: &str) -> DomainResult<Self> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(DomainError::InvalidPickOrder(raw.to_string()));
        }
        let mut pattern = Vec::with_capacity(trimmed.len());
        for ch in trimmed.chars() {
            if !ch.is_ascii_alphabetic() {
                return Err(DomainError::InvalidPickOrder(raw.to_string()));
            }
            pattern.push((ch.to_ascii_uppercase() as u8 - b'A') as usize);
        }
        Ok(Self { pattern })
    }

    /// Rejects an order that references a team the queue does not have.
    ///
    /// Validated at configuration time so a bad pick order fails when it is set
    /// rather than part-way through a draft.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::InvalidPickOrder`] if any letter exceeds
    /// `team_count`.
    pub fn ensure_fits(&self, team_count: usize) -> DomainResult<()> {
        if let Some(&worst) = self.pattern.iter().max() {
            if worst >= team_count {
                return Err(DomainError::InvalidPickOrder(format!(
                    "{} references team {} but the queue has {} teams",
                    self.as_string(),
                    (b'A' + worst as u8) as char,
                    team_count
                )));
            }
        }
        Ok(())
    }

    /// Which team owns pick number `index` (0-based), ignoring full teams.
    ///
    /// The pattern cycles, so an order shorter than the number of picks simply
    /// repeats.
    ///
    /// # Panics
    ///
    /// Panics if the pattern is empty, which [`PickOrder::parse`] prevents.
    #[must_use]
    pub fn team_at(&self, index: usize) -> usize {
        self.pattern[index % self.pattern.len()]
    }

    /// Renders the pattern back to letters, the inverse of
    /// [`PickOrder::parse`].
    #[must_use]
    pub fn as_string(&self) -> String {
        self.pattern
            .iter()
            .map(|&i| (b'A' + i as u8) as char)
            .collect()
    }

    /// How many picks the pattern describes before it repeats.
    #[must_use]
    pub fn len(&self) -> usize {
        self.pattern.len()
    }

    /// Whether the pattern is empty. Always false for a parsed order.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pattern.is_empty()
    }
}

impl fmt::Display for PickOrder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.as_string())
    }
}

impl Serialize for PickOrder {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.as_string())
    }
}

impl<'de> Deserialize<'de> for PickOrder {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        PickOrder::parse(&raw).map_err(D::Error::custom)
    }
}

/// One completed pick, recorded for history and for rebuilding the draft.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pick {
    /// Position in the draft, starting at zero.
    pub seq: usize,
    /// The team the player joined.
    pub team: usize,
    /// The captain who made the pick. `None` for the automatic final
    /// assignment, which nobody chose.
    pub captain: Option<UserId>,
    /// The player who was chosen.
    pub player: UserId,
}

/// Result of applying a pick, so the caller knows what to announce.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickOutcome {
    /// The pick that was requested, plus any automatic assignment that
    /// followed it.
    pub picks: Vec<Pick>,
    /// Whether the draft is now finished.
    pub complete: bool,
}

/// A captain draft in progress.
///
/// Rebuilt from the database on every command rather than held in memory, so a
/// restart mid-draft loses nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Draft {
    /// How many players each team holds when full.
    pub team_size: usize,
    /// `teams[i][0]` is the captain of team `i` once one is set.
    pub teams: Vec<Vec<UserId>>,
    /// The captain of each team, or `None` while the slot is unclaimed.
    pub captains: Vec<Option<UserId>>,
    /// Players still available to be picked.
    pub pool: Vec<UserId>,
    /// The configured pick pattern.
    pub order: PickOrder,
    /// Every pick made so far, in order.
    pub picks: Vec<Pick>,
}

impl Draft {
    /// Starts a draft over `players`, with nobody yet appointed captain.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::InvalidConfig`] if there are fewer than two teams,
    /// a team size of zero, a roster that does not match `team_count * team_size`,
    /// or a duplicated player. Returns [`DomainError::InvalidPickOrder`] if the
    /// order names a team that does not exist.
    pub fn new(
        players: Vec<UserId>,
        team_count: usize,
        team_size: usize,
        order: PickOrder,
    ) -> DomainResult<Self> {
        // Validation is up front so an impossible draft never reaches the
        // database.
        // Validation is up front so an impossible draft never reaches the
        // database.
        if team_count < 2 {
            return Err(DomainError::InvalidConfig(
                "a draft needs at least 2 teams".into(),
            ));
        }
        if team_size == 0 {
            return Err(DomainError::InvalidConfig(
                "a draft needs at least 1 player per team".into(),
            ));
        }
        if players.len() != team_count * team_size {
            return Err(DomainError::InvalidConfig(format!(
                "expected {} players for {team_count}x{team_size}, got {}",
                team_count * team_size,
                players.len()
            )));
        }
        let mut seen = players.clone();
        seen.sort_unstable();
        seen.dedup();
        if seen.len() != players.len() {
            return Err(DomainError::InvalidConfig(
                "the same player appears twice in the draft pool".into(),
            ));
        }
        order.ensure_fits(team_count)?;
        Ok(Self {
            team_size,
            teams: vec![Vec::new(); team_count],
            captains: vec![None; team_count],
            pool: players,
            order,
            picks: Vec::new(),
        })
    }

    /// How many teams the draft fills.
    #[must_use]
    pub fn team_count(&self) -> usize {
        self.teams.len()
    }

    /// Whether every captain slot is filled, which is required before any pick
    /// can be made.
    #[must_use]
    pub fn captains_ready(&self) -> bool {
        self.captains.iter().all(Option::is_some)
    }

    /// Assigns a captain slot. The captain leaves the pool and occupies the
    /// first roster position of the team.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::NoSuchTeam`] for an out-of-range team,
    /// [`DomainError::CaptainSlotTaken`] if the slot is filled,
    /// [`DomainError::InvalidConfig`] if the player already captains another
    /// team, or [`DomainError::PlayerNotInPool`] if they are not available.
    pub fn set_captain(&mut self, team: usize, user: UserId) -> DomainResult<()> {
        if team >= self.team_count() {
            return Err(DomainError::NoSuchTeam(team));
        }
        if self.captains[team].is_some() {
            return Err(DomainError::CaptainSlotTaken(team));
        }
        if self.captains.contains(&Some(user)) {
            return Err(DomainError::InvalidConfig(
                "that player is already a captain".into(),
            ));
        }
        let position = self
            .pool
            .iter()
            .position(|&p| p == user)
            .ok_or(DomainError::PlayerNotInPool(user))?;
        self.pool.remove(position);
        self.captains[team] = Some(user);
        self.teams[team].insert(0, user);
        Ok(())
    }

    /// Releases a captain slot, returning the player to the pool.
    ///
    /// Only allowed before that captain has picked anybody; otherwise their
    /// picks would be orphaned.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::NotActiveCaptain`] if the user captains nothing,
    /// or [`DomainError::InvalidConfig`] if they have already picked.
    pub fn vacate_captain(&mut self, user: UserId) -> DomainResult<usize> {
        let team = self
            .captains
            .iter()
            .position(|c| *c == Some(user))
            .ok_or(DomainError::NotActiveCaptain)?;
        if self.teams[team].len() > 1 {
            return Err(DomainError::InvalidConfig(
                "a captain who has already picked cannot step down".into(),
            ));
        }
        self.captains[team] = None;
        self.teams[team].clear();
        self.pool.push(user);
        Ok(team)
    }

    fn team_is_full(&self, team: usize) -> bool {
        self.teams[team].len() >= self.team_size
    }

    /// The team whose turn it is, skipping teams that are already full.
    ///
    /// Returns `None` when captains are still being chosen or the draft is
    /// finished.
    #[must_use]
    pub fn current_team(&self) -> Option<usize> {
        if !self.captains_ready() || self.is_complete() {
            return None;
        }
        // One cycle of the pattern is enough: if any team named by the order
        // has room, it appears within `order.len()` steps.
        let start = self.picks.len();
        for offset in 0..self.order.len() {
            let team = self.order.team_at(start + offset);
            if !self.team_is_full(team) {
                return Some(team);
            }
        }
        // The order is a preference, not a constraint. A pattern that never
        // names a team with room (say `AAAA` for two teams) must still let the
        // draft finish, so fall back to the first team with a free slot.
        (0..self.team_count()).find(|&team| !self.team_is_full(team))
    }

    /// The captain who is on the clock, if any.
    #[must_use]
    pub fn current_captain(&self) -> Option<UserId> {
        self.current_team().and_then(|team| self.captains[team])
    }

    /// Whether every team is full and the pool is empty.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.pool.is_empty() && self.teams.iter().all(|team| team.len() == self.team_size)
    }

    /// How many picks the current captain would make before the turn passes.
    /// How many roster places are still unfilled across all teams.
    #[must_use]
    pub fn remaining_slots(&self) -> usize {
        self.teams
            .iter()
            .map(|team| self.team_size.saturating_sub(team.len()))
            .sum()
    }

    /// Applies a pick by `captain`.
    ///
    /// When exactly one player would be left afterwards, that player is
    /// assigned automatically rather than making a captain click through a
    /// single option. Both picks are reported in the returned
    /// [`PickOutcome`].
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::InvalidConfig`] if captains are not yet chosen,
    /// [`DomainError::DraftComplete`] if there is nothing left to pick,
    /// [`DomainError::NotActiveCaptain`] if it is not this captain's turn, or
    /// [`DomainError::PlayerNotInPool`] if the target is unavailable.
    pub fn pick(&mut self, captain: UserId, player: UserId) -> DomainResult<PickOutcome> {
        if !self.captains_ready() {
            return Err(DomainError::InvalidConfig(
                "captains have not been chosen yet".into(),
            ));
        }
        let team = self.current_team().ok_or(DomainError::DraftComplete)?;
        if self.captains[team] != Some(captain) {
            return Err(DomainError::NotActiveCaptain);
        }
        let position = self
            .pool
            .iter()
            .position(|&p| p == player)
            .ok_or(DomainError::PlayerNotInPool(player))?;
        self.pool.remove(position);
        self.teams[team].push(player);
        let mut picks = vec![Pick {
            seq: self.picks.len(),
            team,
            captain: Some(captain),
            player,
        }];
        self.picks.push(picks[0].clone());

        // A one-player pool leaves no choice; assign it rather than making a
        // captain click through a single option.
        if self.pool.len() == 1 {
            if let Some(last_team) = self.current_team() {
                let player = self.pool.remove(0);
                self.teams[last_team].push(player);
                let auto = Pick {
                    seq: self.picks.len(),
                    team: last_team,
                    captain: None,
                    player,
                };
                self.picks.push(auto.clone());
                picks.push(auto);
            }
        }

        Ok(PickOutcome {
            complete: self.is_complete(),
            picks,
        })
    }

    /// Moderator override: place a player on a team, or back in the pool,
    /// regardless of whose turn it is.
    ///
    /// Removing a captain from their team also vacates the captain slot.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::NoSuchTeam`] for an out-of-range team, or
    /// [`DomainError::NotInMatch`] if the player is not in this draft.
    pub fn force_place(&mut self, player: UserId, team: Option<usize>) -> DomainResult<()> {
        if let Some(team) = team {
            if team >= self.team_count() {
                return Err(DomainError::NoSuchTeam(team));
            }
        }
        let mut found = false;
        if let Some(index) = self.pool.iter().position(|&p| p == player) {
            self.pool.remove(index);
            found = true;
        }
        for (index, roster) in self.teams.iter_mut().enumerate() {
            if let Some(position) = roster.iter().position(|&p| p == player) {
                roster.remove(position);
                if self.captains[index] == Some(player) {
                    self.captains[index] = None;
                }
                found = true;
            }
        }
        if !found {
            return Err(DomainError::NotInMatch);
        }
        match team {
            Some(team) => self.teams[team].push(player),
            None => self.pool.push(player),
        }
        Ok(())
    }

    /// Replaces `out` with `into` wherever `out` sits, preserving captaincy.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::AlreadyInMatch`] if `into` is already involved,
    /// or [`DomainError::NotInMatch`] if `out` is not.
    pub fn substitute(&mut self, out: UserId, into: UserId) -> DomainResult<()> {
        if self.pool.contains(&into) || self.teams.iter().any(|t| t.contains(&into)) {
            return Err(DomainError::AlreadyInMatch);
        }
        if let Some(index) = self.pool.iter().position(|&p| p == out) {
            self.pool[index] = into;
            return Ok(());
        }
        for (team_index, roster) in self.teams.iter_mut().enumerate() {
            if let Some(index) = roster.iter().position(|&p| p == out) {
                roster[index] = into;
                if self.captains[team_index] == Some(out) {
                    self.captains[team_index] = Some(into);
                }
                return Ok(());
            }
        }
        Err(DomainError::NotInMatch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn users(n: usize) -> Vec<UserId> {
        (1..=n as i64).map(UserId).collect()
    }

    fn draft_of(n: usize, order: &str) -> Draft {
        let mut draft = Draft::new(users(n), 2, n / 2, PickOrder::parse(order).unwrap()).unwrap();
        draft.set_captain(0, UserId(1)).unwrap();
        draft.set_captain(1, UserId(2)).unwrap();
        draft
    }

    #[test]
    fn pick_order_parses_and_round_trips() {
        let order = PickOrder::parse("abbaab").unwrap();
        assert_eq!(order.as_string(), "ABBAAB");
        assert_eq!(order.team_at(0), 0);
        assert_eq!(order.team_at(1), 1);
        assert_eq!(order.team_at(6), 0, "the pattern cycles");
    }

    #[test]
    fn pick_order_rejects_non_letters_and_missing_teams() {
        assert!(PickOrder::parse("AB1").is_err());
        assert!(PickOrder::parse("").is_err());
        assert!(PickOrder::parse("ABC").unwrap().ensure_fits(2).is_err());
        assert!(PickOrder::parse("ABC").unwrap().ensure_fits(3).is_ok());
    }

    #[test]
    fn abba_order_gives_the_second_captain_two_picks_in_a_row() {
        let mut draft = draft_of(10, "ABBA");
        assert_eq!(draft.current_team(), Some(0));
        draft.pick(UserId(1), UserId(3)).unwrap();
        assert_eq!(draft.current_team(), Some(1));
        draft.pick(UserId(2), UserId(4)).unwrap();
        assert_eq!(draft.current_team(), Some(1));
        draft.pick(UserId(2), UserId(5)).unwrap();
        assert_eq!(draft.current_team(), Some(0));
    }

    #[test]
    fn only_the_active_captain_can_pick() {
        let mut draft = draft_of(10, "AB");
        assert_eq!(
            draft.pick(UserId(2), UserId(3)).unwrap_err(),
            DomainError::NotActiveCaptain
        );
        // A non-captain is rejected the same way.
        assert_eq!(
            draft.pick(UserId(7), UserId(3)).unwrap_err(),
            DomainError::NotActiveCaptain
        );
    }

    #[test]
    fn a_player_cannot_be_picked_twice() {
        let mut draft = draft_of(10, "AB");
        draft.pick(UserId(1), UserId(3)).unwrap();
        assert_eq!(
            draft.pick(UserId(2), UserId(3)).unwrap_err(),
            DomainError::PlayerNotInPool(UserId(3))
        );
    }

    #[test]
    fn captains_cannot_be_picked_because_they_left_the_pool() {
        let mut draft = draft_of(10, "AB");
        assert_eq!(
            draft.pick(UserId(1), UserId(2)).unwrap_err(),
            DomainError::PlayerNotInPool(UserId(2))
        );
    }

    #[test]
    fn the_final_player_is_assigned_automatically() {
        // 3v3: two captains plus four in the pool, so the third pick is the
        // last real choice and the fourth player has nowhere else to go.
        let mut draft = draft_of(6, "AB");
        draft.pick(UserId(1), UserId(3)).unwrap();
        draft.pick(UserId(2), UserId(4)).unwrap();
        let outcome = draft.pick(UserId(1), UserId(5)).unwrap();
        assert_eq!(outcome.picks.len(), 2, "the forced pick is reported too");
        assert_eq!(outcome.picks[1].player, UserId(6));
        assert_eq!(outcome.picks[1].team, 1);
        assert!(outcome.picks[1].captain.is_none());
        assert!(outcome.complete);
        assert!(draft.pool.is_empty());
    }

    #[test]
    fn every_pick_order_terminates_with_full_teams() {
        for order in ["AB", "ABBA", "ABABABBA", "AABB", "ABBABAAB"] {
            for size in [4usize, 6, 8, 10, 12] {
                let mut draft = draft_of(size, order);
                let mut guard = 0;
                while !draft.is_complete() {
                    guard += 1;
                    assert!(
                        guard < 100,
                        "draft {order} at size {size} did not terminate"
                    );
                    let team = draft
                        .current_team()
                        .expect("an incomplete draft has a turn");
                    let captain = draft.captains[team].unwrap();
                    let next = *draft.pool.first().expect("pool is non-empty");
                    draft.pick(captain, next).unwrap();
                }
                assert!(draft.pool.is_empty());
                for roster in &draft.teams {
                    assert_eq!(roster.len(), size / 2, "order {order} left an uneven team");
                }
                let mut all: Vec<UserId> = draft.teams.concat();
                all.sort_unstable();
                assert_eq!(
                    all,
                    users(size),
                    "order {order} lost or duplicated a player"
                );
            }
        }
    }

    #[test]
    fn a_lopsided_order_still_fills_both_teams() {
        // AAAA would overfill team A; the turn must skip to B once A is full.
        let mut draft = draft_of(8, "AAAA");
        while !draft.is_complete() {
            let team = draft.current_team().unwrap();
            let captain = draft.captains[team].unwrap();
            let next = *draft.pool.first().unwrap();
            draft.pick(captain, next).unwrap();
        }
        assert_eq!(draft.teams[0].len(), 4);
        assert_eq!(draft.teams[1].len(), 4);
    }

    #[test]
    fn captains_can_step_down_before_picking_but_not_after() {
        let mut draft = draft_of(10, "AB");
        assert_eq!(draft.vacate_captain(UserId(1)).unwrap(), 0);
        assert!(draft.pool.contains(&UserId(1)));
        assert!(
            draft.current_team().is_none(),
            "the draft waits for captains"
        );

        draft.set_captain(0, UserId(9)).unwrap();
        draft.pick(UserId(9), UserId(1)).unwrap();
        assert!(draft.vacate_captain(UserId(9)).is_err());
    }

    #[test]
    fn a_taken_captain_slot_is_rejected() {
        let mut draft = draft_of(10, "AB");
        assert_eq!(
            draft.set_captain(0, UserId(5)).unwrap_err(),
            DomainError::CaptainSlotTaken(0)
        );
        assert_eq!(
            draft.set_captain(5, UserId(5)).unwrap_err(),
            DomainError::NoSuchTeam(5)
        );
    }

    #[test]
    fn substitution_preserves_the_captain_slot() {
        let mut draft = draft_of(10, "AB");
        draft.substitute(UserId(1), UserId(99)).unwrap();
        assert_eq!(draft.captains[0], Some(UserId(99)));
        assert_eq!(draft.teams[0][0], UserId(99));
        assert_eq!(
            draft.substitute(UserId(1), UserId(50)).unwrap_err(),
            DomainError::NotInMatch,
            "the player being replaced must actually be in the match"
        );
        assert_eq!(
            draft.substitute(UserId(99), UserId(3)).unwrap_err(),
            DomainError::AlreadyInMatch,
            "the replacement must not already be in the match"
        );
    }

    #[test]
    fn a_duplicate_roster_is_refused_at_construction() {
        let players = vec![UserId(1), UserId(1), UserId(2), UserId(3)];
        assert!(Draft::new(players, 2, 2, PickOrder::default()).is_err());
    }

    #[test]
    fn force_place_moves_a_player_between_team_and_pool() {
        let mut draft = draft_of(10, "AB");
        draft.force_place(UserId(5), Some(1)).unwrap();
        assert!(draft.teams[1].contains(&UserId(5)));
        draft.force_place(UserId(5), None).unwrap();
        assert!(draft.pool.contains(&UserId(5)));
        assert!(!draft.teams[1].contains(&UserId(5)));
    }
}
