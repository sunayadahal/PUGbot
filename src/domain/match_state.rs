//! The match lifecycle state machine.
//!
//! Every transition a command wants to perform is declared here rather than
//! being implied by scattered handler code. Persistence pairs each state with a
//! monotonically increasing `version` used for optimistic locking.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::{DomainError, DomainResult};

/// The lifecycle state of a match.
///
/// The legal transitions are declared by [`MatchState::allowed_next`] rather
/// than being implied by scattered handler code, so the whole state machine can
/// be read — and tested — in one place. Persistence pairs each state with a
/// monotonically increasing version used for optimistic locking.
///
/// # Example
///
/// ```
/// use pugbot::domain::match_state::MatchState;
///
/// assert!(MatchState::CheckIn.can_transition_to(MatchState::TeamFormation));
/// assert!(MatchState::CheckIn.ensure_transition(MatchState::Completed).is_err());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MatchState {
    /// Roster assembled from the queue; nothing has been asked of players yet.
    Queued,
    /// Ready-check in progress.
    CheckIn,
    /// Captains drafting, or teams being computed.
    TeamFormation,
    /// Players voting on maps.
    MapVote,
    /// Match is being played.
    Active,
    /// A result was reported and is awaiting consensus or moderator action.
    ReportPending,
    /// Finished with a recorded result.
    Completed,
    /// Aborted before completion; never rated.
    Cancelled,
    /// Exceeded its configured lifetime; never rated.
    Expired,
}

impl MatchState {
    /// Every state, in lifecycle order. Useful for exhaustive tests and for
    /// parsing.
    pub const ALL: [MatchState; 9] = [
        MatchState::Queued,
        MatchState::CheckIn,
        MatchState::TeamFormation,
        MatchState::MapVote,
        MatchState::Active,
        MatchState::ReportPending,
        MatchState::Completed,
        MatchState::Cancelled,
        MatchState::Expired,
    ];

    /// The stable string stored in the `matches.state` column.
    ///
    /// This is part of the database schema — a `CHECK` constraint lists these
    /// exact values — so it must not be changed without a migration.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            MatchState::Queued => "QUEUED",
            MatchState::CheckIn => "CHECK_IN",
            MatchState::TeamFormation => "TEAM_FORMATION",
            MatchState::MapVote => "MAP_VOTE",
            MatchState::Active => "ACTIVE",
            MatchState::ReportPending => "REPORT_PENDING",
            MatchState::Completed => "COMPLETED",
            MatchState::Cancelled => "CANCELLED",
            MatchState::Expired => "EXPIRED",
        }
    }

    /// Parses the stored string form, the inverse of [`MatchState::as_str`].
    ///
    /// Returns `None` for anything unrecognised, which the repository treats as
    /// a corrupt row rather than guessing.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        MatchState::ALL.into_iter().find(|s| s.as_str() == value)
    }

    /// Whether the match has finished, one way or another.
    ///
    /// A terminal state can only be left through an audited administrative
    /// correction, which is modelled as a new record rather than a transition.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            MatchState::Completed | MatchState::Cancelled | MatchState::Expired
        )
    }

    /// Whether the match still occupies its players.
    ///
    /// A live match blocks its roster from joining another queue within the
    /// channel's configured scope.
    #[must_use]
    pub const fn is_live(self) -> bool {
        !self.is_terminal()
    }

    /// The states this one may transition into.
    ///
    /// This is the single authoritative description of the state machine.
    #[must_use]
    pub const fn allowed_next(self) -> &'static [MatchState] {
        use MatchState::*;
        match self {
            Queued => &[CheckIn, TeamFormation, Cancelled, Expired],
            CheckIn => &[TeamFormation, Cancelled, Expired],
            TeamFormation => &[MapVote, Active, Cancelled, Expired],
            MapVote => &[Active, Cancelled, Expired],
            Active => &[ReportPending, Completed, Cancelled, Expired],
            ReportPending => &[Active, Completed, Cancelled, Expired],
            Completed | Cancelled | Expired => &[],
        }
    }

    /// Whether moving to `to` is legal from this state.
    #[must_use]
    pub fn can_transition_to(self, to: MatchState) -> bool {
        self.allowed_next().contains(&to)
    }

    /// Asserts that moving to `to` is legal.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::InvalidTransition`] if the move is not declared
    /// by [`MatchState::allowed_next`].
    pub fn ensure_transition(self, to: MatchState) -> DomainResult<()> {
        if self.can_transition_to(to) {
            Ok(())
        } else {
            Err(DomainError::InvalidTransition { from: self, to })
        }
    }

    /// Asserts that the match is in exactly the state a command requires.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::UnexpectedState`] if the states differ.
    pub fn ensure_is(self, expected: MatchState) -> DomainResult<()> {
        if self == expected {
            Ok(())
        } else {
            Err(DomainError::UnexpectedState {
                expected,
                actual: self,
            })
        }
    }
}

impl fmt::Display for MatchState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_state_round_trips_through_its_string_form() {
        for state in MatchState::ALL {
            assert_eq!(MatchState::parse(state.as_str()), Some(state));
        }
        assert_eq!(MatchState::parse("NOPE"), None);
    }

    #[test]
    fn terminal_states_have_no_successors() {
        for state in MatchState::ALL {
            if state.is_terminal() {
                assert!(state.allowed_next().is_empty(), "{state} should be final");
            }
        }
    }

    #[test]
    fn happy_path_is_reachable() {
        let path = [
            MatchState::Queued,
            MatchState::CheckIn,
            MatchState::TeamFormation,
            MatchState::MapVote,
            MatchState::Active,
            MatchState::ReportPending,
            MatchState::Completed,
        ];
        for pair in path.windows(2) {
            pair[0].ensure_transition(pair[1]).expect("valid step");
        }
    }

    #[test]
    fn completed_match_cannot_be_reopened() {
        let err = MatchState::Completed
            .ensure_transition(MatchState::Active)
            .unwrap_err();
        assert_eq!(
            err,
            DomainError::InvalidTransition {
                from: MatchState::Completed,
                to: MatchState::Active
            }
        );
    }

    #[test]
    fn a_disputed_report_can_return_to_active() {
        assert!(MatchState::ReportPending.can_transition_to(MatchState::Active));
    }

    #[test]
    fn every_live_state_can_be_cancelled() {
        for state in MatchState::ALL.into_iter().filter(|s| s.is_live()) {
            assert!(
                state.can_transition_to(MatchState::Cancelled),
                "{state} must be cancellable by a moderator"
            );
        }
    }
}
