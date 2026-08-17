//! Result reporting and consensus.
//!
//! A result is final when every team has at least one member reporting the
//! same outcome. Teams that disagree put the match into a disputed state that
//! only a moderator can settle, which keeps ratings out of the hands of a
//! single player.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::domain::ids::UserId;
use crate::error::{DomainError, DomainResult};

/// A result somebody has reported, or that a moderator has imposed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportOutcome {
    /// The identified team won.
    Win(usize),
    /// Neither side won.
    Draw,
    /// The match did not happen; nobody is rated.
    Cancel,
}

impl ReportOutcome {
    /// Whether this outcome should move ratings.
    ///
    /// A cancellation is never rated: there is no result to learn from.
    #[must_use]
    pub fn is_rated(self) -> bool {
        !matches!(self, ReportOutcome::Cancel)
    }
}

/// The state of the consensus process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Consensus {
    /// Not every team has reported yet.
    Pending {
        /// Indices of the teams that have not reported.
        missing_teams: Vec<usize>,
    },
    /// Every team agrees.
    Agreed(ReportOutcome),
    /// Teams reported incompatible outcomes.
    Disputed,
}

/// Every report collected for one match.
///
/// Ordered by user so the same set of reports always evaluates identically,
/// which keeps [`ReportLedger::evaluate`] deterministic.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportLedger {
    /// The latest outcome each player reported. Re-reporting replaces.
    pub reports: BTreeMap<UserId, ReportOutcome>,
    /// Optional per-team scores from the reporter, kept for history.
    pub scores: Option<Vec<i32>>,
}

impl ReportLedger {
    /// Records or replaces one player's report.
    pub fn record(&mut self, user: UserId, outcome: ReportOutcome) {
        self.reports.insert(user, outcome);
    }

    /// Attaches per-team scores, indexed by team number.
    pub fn set_scores(&mut self, scores: Vec<i32>) {
        self.scores = Some(scores);
    }

    /// Evaluates consensus over `rosters`, where `rosters[i]` is team `i`.
    ///
    /// A result is agreed when every team has at least one member reporting the
    /// same outcome. Teams that disagree — or teammates who contradict each
    /// other — produce [`Consensus::Disputed`], which only a moderator can
    /// settle. That keeps ratings out of the hands of any single player.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::InvalidConfig`] if fewer than two rosters are
    /// supplied, since consensus is meaningless without an opposing side.
    ///
    /// # Panics
    ///
    /// Never: the unwrap after the missing-team check is guarded by that check.
    pub fn evaluate(&self, rosters: &[Vec<UserId>]) -> DomainResult<Consensus> {
        if rosters.len() < 2 {
            return Err(DomainError::InvalidConfig(
                "consensus needs at least two teams".into(),
            ));
        }

        let mut per_team: Vec<Option<ReportOutcome>> = vec![None; rosters.len()];
        let mut missing_teams = Vec::new();

        for (index, roster) in rosters.iter().enumerate() {
            let mut team_outcomes: Vec<ReportOutcome> = roster
                .iter()
                .filter_map(|user| self.reports.get(user).copied())
                .collect();
            team_outcomes.dedup();
            match team_outcomes.first() {
                None => missing_teams.push(index),
                Some(&first) => {
                    // Members of one team contradicting each other is treated
                    // as a dispute for the whole match.
                    if team_outcomes.iter().any(|o| *o != first) {
                        return Ok(Consensus::Disputed);
                    }
                    per_team[index] = Some(first);
                }
            }
        }

        if !missing_teams.is_empty() {
            return Ok(Consensus::Pending { missing_teams });
        }

        let first = per_team[0].expect("checked above");
        if per_team.iter().all(|outcome| *outcome == Some(first)) {
            Ok(Consensus::Agreed(first))
        } else {
            Ok(Consensus::Disputed)
        }
    }

    /// Whether anybody has reported at all.
    /// Whether nobody has reported yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.reports.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rosters() -> Vec<Vec<UserId>> {
        vec![vec![UserId(1), UserId(2)], vec![UserId(3), UserId(4)]]
    }

    #[test]
    fn a_single_report_leaves_the_other_team_outstanding() {
        let mut ledger = ReportLedger::default();
        ledger.record(UserId(1), ReportOutcome::Win(0));
        assert_eq!(
            ledger.evaluate(&rosters()).unwrap(),
            Consensus::Pending {
                missing_teams: vec![1]
            }
        );
    }

    #[test]
    fn matching_reports_from_both_teams_agree() {
        let mut ledger = ReportLedger::default();
        ledger.record(UserId(1), ReportOutcome::Win(0));
        ledger.record(UserId(4), ReportOutcome::Win(0));
        assert_eq!(
            ledger.evaluate(&rosters()).unwrap(),
            Consensus::Agreed(ReportOutcome::Win(0))
        );
    }

    #[test]
    fn opposing_claims_are_disputed() {
        let mut ledger = ReportLedger::default();
        ledger.record(UserId(1), ReportOutcome::Win(0));
        ledger.record(UserId(4), ReportOutcome::Win(1));
        assert_eq!(ledger.evaluate(&rosters()).unwrap(), Consensus::Disputed);
    }

    #[test]
    fn teammates_contradicting_each_other_is_a_dispute() {
        let mut ledger = ReportLedger::default();
        ledger.record(UserId(1), ReportOutcome::Win(0));
        ledger.record(UserId(2), ReportOutcome::Draw);
        assert_eq!(ledger.evaluate(&rosters()).unwrap(), Consensus::Disputed);
    }

    #[test]
    fn a_player_changing_their_mind_replaces_their_report() {
        let mut ledger = ReportLedger::default();
        ledger.record(UserId(1), ReportOutcome::Win(0));
        ledger.record(UserId(1), ReportOutcome::Draw);
        ledger.record(UserId(3), ReportOutcome::Draw);
        assert_eq!(
            ledger.evaluate(&rosters()).unwrap(),
            Consensus::Agreed(ReportOutcome::Draw)
        );
    }

    #[test]
    fn both_teams_agreeing_to_cancel_is_consensus() {
        let mut ledger = ReportLedger::default();
        ledger.record(UserId(2), ReportOutcome::Cancel);
        ledger.record(UserId(3), ReportOutcome::Cancel);
        assert_eq!(
            ledger.evaluate(&rosters()).unwrap(),
            Consensus::Agreed(ReportOutcome::Cancel)
        );
        assert!(!ReportOutcome::Cancel.is_rated());
        assert!(ReportOutcome::Draw.is_rated());
        assert!(ReportOutcome::Win(1).is_rated());
    }

    #[test]
    fn reports_from_outside_the_rosters_are_ignored() {
        let mut ledger = ReportLedger::default();
        ledger.record(UserId(99), ReportOutcome::Win(0));
        assert_eq!(
            ledger.evaluate(&rosters()).unwrap(),
            Consensus::Pending {
                missing_teams: vec![0, 1]
            }
        );
    }

    #[test]
    fn three_teams_all_have_to_agree() {
        let rosters = vec![vec![UserId(1)], vec![UserId(2)], vec![UserId(3)]];
        let mut ledger = ReportLedger::default();
        ledger.record(UserId(1), ReportOutcome::Win(0));
        ledger.record(UserId(2), ReportOutcome::Win(0));
        assert_eq!(
            ledger.evaluate(&rosters).unwrap(),
            Consensus::Pending {
                missing_teams: vec![2]
            }
        );
        ledger.record(UserId(3), ReportOutcome::Win(0));
        assert_eq!(
            ledger.evaluate(&rosters).unwrap(),
            Consensus::Agreed(ReportOutcome::Win(0))
        );
    }

    #[test]
    fn consensus_requires_at_least_two_teams() {
        let ledger = ReportLedger::default();
        assert!(ledger.evaluate(&[vec![UserId(1)]]).is_err());
    }
}
