//! Ready-check state and its failure policy.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::domain::ids::UserId;
use crate::domain::settings::{CheckInReturnPolicy, CheckInSettings};
use crate::error::{DomainError, DomainResult};

/// One player's answer to a ready-check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadyState {
    /// Has not answered yet.
    Pending,
    /// Confirmed they are ready to play.
    Ready,
    /// Declined the match.
    Declined,
}

impl ReadyState {
    /// The stable string stored in `match_players.ready_state`.
    ///
    /// A `CHECK` constraint lists these exact values, so changing them requires
    /// a migration.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            ReadyState::Pending => "pending",
            ReadyState::Ready => "ready",
            ReadyState::Declined => "declined",
        }
    }

    /// Parses the stored form, the inverse of [`ReadyState::as_str`].
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "pending" => Some(ReadyState::Pending),
            "ready" => Some(ReadyState::Ready),
            "declined" => Some(ReadyState::Declined),
            _ => None,
        }
    }
}

/// A ready-check in progress.
///
/// Rebuilt from the database on each command, so a restart mid-check-in loses
/// nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckIn {
    /// When the check-in stops accepting answers.
    pub deadline: DateTime<Utc>,
    /// Ordered so the embed always lists players the same way.
    pub states: BTreeMap<UserId, ReadyState>,
}

/// What the caller should do with a check-in right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckInOutcome {
    /// Still collecting responses.
    Waiting,
    /// Everybody is ready; proceed to team formation.
    Passed,
    /// The check-in failed. `returned` go back into the queue, `dropped` do not.
    Failed {
        /// Players who go back into the queue, per the configured policy.
        returned: Vec<UserId>,
        /// Players who do not. Together with `returned` this covers the whole
        /// roster exactly once.
        dropped: Vec<UserId>,
    },
}

impl CheckIn {
    /// Opens a check-in over `players`, all of them pending.
    #[must_use]
    pub fn new(players: &[UserId], deadline: DateTime<Utc>) -> Self {
        Self {
            deadline,
            states: players
                .iter()
                .map(|&user| (user, ReadyState::Pending))
                .collect(),
        }
    }

    /// Marks players whose auto-ready preference is armed as already ready.
    ///
    /// Only pending players are upgraded: somebody who has explicitly declined
    /// is not overridden by a preference they set earlier.
    pub fn apply_auto_ready(&mut self, users: &[UserId]) {
        for user in users {
            if let Some(state) = self.states.get_mut(user) {
                if *state == ReadyState::Pending {
                    *state = ReadyState::Ready;
                }
            }
        }
    }

    /// Records one player's answer.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::NotInMatch`] if the user is not on the roster.
    pub fn set(&mut self, user: UserId, state: ReadyState) -> DomainResult<()> {
        match self.states.get_mut(&user) {
            Some(slot) => {
                *slot = state;
                Ok(())
            }
            None => Err(DomainError::NotInMatch),
        }
    }

    /// Replaces a player without losing the responses already collected.
    ///
    /// The substitute starts pending, whatever the player they replace had
    /// answered — they have not agreed to anything yet.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::AlreadyInMatch`] if `into` is already on the
    /// roster, or [`DomainError::NotInMatch`] if `out` is not.
    pub fn substitute(&mut self, out: UserId, into: UserId) -> DomainResult<()> {
        if self.states.contains_key(&into) {
            return Err(DomainError::AlreadyInMatch);
        }
        self.states.remove(&out).ok_or(DomainError::NotInMatch)?;
        // A substitute has not answered yet, whatever the player they replace
        // had said.
        self.states.insert(into, ReadyState::Pending);
        Ok(())
    }

    /// The players currently in `wanted`, in a stable order.
    #[must_use]
    pub fn by_state(&self, wanted: ReadyState) -> Vec<UserId> {
        self.states
            .iter()
            .filter(|(_, state)| **state == wanted)
            .map(|(user, _)| *user)
            .collect()
    }

    /// Players who confirmed.
    #[must_use]
    pub fn ready(&self) -> Vec<UserId> {
        self.by_state(ReadyState::Ready)
    }

    /// Players who have not answered.
    #[must_use]
    pub fn pending(&self) -> Vec<UserId> {
        self.by_state(ReadyState::Pending)
    }

    /// Players who declined.
    #[must_use]
    pub fn declined(&self) -> Vec<UserId> {
        self.by_state(ReadyState::Declined)
    }

    /// Whether every player has confirmed, which lets the check-in pass early.
    #[must_use]
    pub fn all_ready(&self) -> bool {
        !self.states.is_empty()
            && self
                .states
                .values()
                .all(|state| *state == ReadyState::Ready)
    }

    /// Whether anybody has declined.
    #[must_use]
    pub fn any_declined(&self) -> bool {
        self.states.values().any(|s| *s == ReadyState::Declined)
    }

    /// Seconds left before the deadline, floored at zero so a late call never
    /// renders a negative countdown.
    #[must_use]
    pub fn seconds_remaining(&self, now: DateTime<Utc>) -> i64 {
        (self.deadline - now).num_seconds().max(0)
    }

    /// Whether the deadline has passed.
    #[must_use]
    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        now >= self.deadline
    }

    /// Evaluates the check-in against its settings.
    ///
    /// This is a pure function of the collected answers, the settings, and the
    /// current time, so the timer job and an interactive ready press reach the
    /// same conclusion.
    ///
    /// Failure returns two disjoint lists that together cover every player, so
    /// the caller can requeue and notify without recomputing the policy.
    pub fn evaluate(&self, settings: &CheckInSettings, now: DateTime<Utc>) -> CheckInOutcome {
        if self.all_ready() {
            return CheckInOutcome::Passed;
        }
        let failed_early = settings.abort_on_decline && self.any_declined();
        if !failed_early && !self.is_expired(now) {
            return CheckInOutcome::Waiting;
        }

        let mut returned = Vec::new();
        let mut dropped = Vec::new();
        for (user, state) in &self.states {
            let keep = match (settings.return_policy, state) {
                (CheckInReturnPolicy::None, _) => false,
                // Decliners are never returned: they said no.
                (_, ReadyState::Declined) => false,
                (CheckInReturnPolicy::ReadyOnly, ReadyState::Ready) => true,
                (CheckInReturnPolicy::ReadyOnly, ReadyState::Pending) => false,
                (CheckInReturnPolicy::ReadyAndPending, _) => true,
            };
            if keep {
                returned.push(*user);
            } else {
                dropped.push(*user);
            }
        }
        CheckInOutcome::Failed { returned, dropped }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-01-01T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn check_in() -> CheckIn {
        CheckIn::new(
            &[UserId(1), UserId(2), UserId(3)],
            now() + Duration::seconds(60),
        )
    }

    fn settings(policy: CheckInReturnPolicy, abort_on_decline: bool) -> CheckInSettings {
        CheckInSettings {
            timeout_seconds: 60,
            abort_on_decline,
            return_policy: policy,
        }
    }

    #[test]
    fn a_fresh_check_in_is_all_pending() {
        let check_in = check_in();
        assert_eq!(check_in.pending().len(), 3);
        assert!(!check_in.all_ready());
        assert_eq!(
            check_in.evaluate(&settings(CheckInReturnPolicy::ReadyOnly, true), now()),
            CheckInOutcome::Waiting
        );
    }

    #[test]
    fn everybody_ready_passes_immediately() {
        let mut check_in = check_in();
        for id in 1..=3 {
            check_in.set(UserId(id), ReadyState::Ready).unwrap();
        }
        assert_eq!(
            check_in.evaluate(&settings(CheckInReturnPolicy::ReadyOnly, true), now()),
            CheckInOutcome::Passed
        );
    }

    #[test]
    fn a_decline_aborts_immediately_when_configured() {
        let mut check_in = check_in();
        check_in.set(UserId(1), ReadyState::Ready).unwrap();
        check_in.set(UserId(2), ReadyState::Declined).unwrap();

        let outcome =
            check_in.evaluate(&settings(CheckInReturnPolicy::ReadyAndPending, true), now());
        assert_eq!(
            outcome,
            CheckInOutcome::Failed {
                returned: vec![UserId(1), UserId(3)],
                dropped: vec![UserId(2)],
            }
        );
    }

    #[test]
    fn a_decline_waits_for_the_timeout_when_not_configured_to_abort() {
        let mut check_in = check_in();
        check_in.set(UserId(2), ReadyState::Declined).unwrap();
        assert_eq!(
            check_in.evaluate(&settings(CheckInReturnPolicy::ReadyOnly, false), now()),
            CheckInOutcome::Waiting
        );
    }

    #[test]
    fn timeout_applies_the_return_policy() {
        let mut check_in = check_in();
        check_in.set(UserId(1), ReadyState::Ready).unwrap();
        check_in.set(UserId(3), ReadyState::Declined).unwrap();
        let late = now() + Duration::seconds(61);

        assert_eq!(
            check_in.evaluate(&settings(CheckInReturnPolicy::ReadyOnly, false), late),
            CheckInOutcome::Failed {
                returned: vec![UserId(1)],
                dropped: vec![UserId(2), UserId(3)],
            }
        );
        assert_eq!(
            check_in.evaluate(&settings(CheckInReturnPolicy::ReadyAndPending, false), late),
            CheckInOutcome::Failed {
                returned: vec![UserId(1), UserId(2)],
                dropped: vec![UserId(3)],
            }
        );
        assert_eq!(
            check_in.evaluate(&settings(CheckInReturnPolicy::None, false), late),
            CheckInOutcome::Failed {
                returned: vec![],
                dropped: vec![UserId(1), UserId(2), UserId(3)],
            }
        );
    }

    #[test]
    fn a_decliner_is_never_returned_to_the_queue() {
        let mut check_in = check_in();
        check_in.set(UserId(1), ReadyState::Declined).unwrap();
        let late = now() + Duration::seconds(61);
        for policy in [
            CheckInReturnPolicy::ReadyOnly,
            CheckInReturnPolicy::ReadyAndPending,
            CheckInReturnPolicy::None,
        ] {
            match check_in.evaluate(&settings(policy, false), late) {
                CheckInOutcome::Failed { returned, dropped } => {
                    assert!(!returned.contains(&UserId(1)), "{policy:?}");
                    assert!(dropped.contains(&UserId(1)), "{policy:?}");
                }
                other => panic!("expected failure, got {other:?}"),
            }
        }
    }

    #[test]
    fn failure_lists_partition_the_roster() {
        let mut check_in = check_in();
        check_in.set(UserId(1), ReadyState::Ready).unwrap();
        check_in.set(UserId(2), ReadyState::Declined).unwrap();
        let late = now() + Duration::seconds(61);
        for policy in [
            CheckInReturnPolicy::ReadyOnly,
            CheckInReturnPolicy::ReadyAndPending,
            CheckInReturnPolicy::None,
        ] {
            let CheckInOutcome::Failed { returned, dropped } =
                check_in.evaluate(&settings(policy, false), late)
            else {
                panic!("expected failure");
            };
            let mut all: Vec<UserId> = returned.into_iter().chain(dropped).collect();
            all.sort_unstable();
            assert_eq!(all, vec![UserId(1), UserId(2), UserId(3)], "{policy:?}");
        }
    }

    #[test]
    fn auto_ready_only_upgrades_players_who_have_not_answered() {
        let mut check_in = check_in();
        check_in.set(UserId(1), ReadyState::Declined).unwrap();
        check_in.apply_auto_ready(&[UserId(1), UserId(2), UserId(99)]);
        assert_eq!(check_in.states[&UserId(1)], ReadyState::Declined);
        assert_eq!(check_in.states[&UserId(2)], ReadyState::Ready);
        assert_eq!(check_in.states[&UserId(3)], ReadyState::Pending);
    }

    #[test]
    fn responses_from_outsiders_are_rejected() {
        let mut check_in = check_in();
        assert_eq!(
            check_in.set(UserId(42), ReadyState::Ready).unwrap_err(),
            DomainError::NotInMatch
        );
    }

    #[test]
    fn a_substitute_starts_pending() {
        let mut check_in = check_in();
        check_in.set(UserId(1), ReadyState::Ready).unwrap();
        check_in.substitute(UserId(1), UserId(9)).unwrap();
        assert_eq!(check_in.states[&UserId(9)], ReadyState::Pending);
        assert!(!check_in.states.contains_key(&UserId(1)));
        assert_eq!(
            check_in.substitute(UserId(9), UserId(2)).unwrap_err(),
            DomainError::AlreadyInMatch
        );
    }

    #[test]
    fn remaining_time_never_goes_negative() {
        let check_in = check_in();
        assert_eq!(check_in.seconds_remaining(now()), 60);
        assert_eq!(check_in.seconds_remaining(now() + Duration::hours(1)), 0);
        assert!(check_in.is_expired(now() + Duration::hours(1)));
    }
}
