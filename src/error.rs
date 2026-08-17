//! Domain and application error types.
//!
//! `thiserror` is used for library-style errors that callers match on; `anyhow`
//! is reserved for the executable boundary in `main.rs`.

use thiserror::Error;

use crate::domain::ids::{ChannelId, MatchId, UserId};
use crate::domain::match_state::MatchState;

/// Errors produced by the pure domain layer. These never touch I/O.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum DomainError {
    /// The player already holds a slot in this queue.
    #[error("player is already in this queue")]
    AlreadyQueued,
    /// The player has no slot to give up.
    #[error("player is not in this queue")]
    NotQueued,
    /// The queue has reached its configured size.
    #[error("queue is full")]
    QueueFull,
    /// The player is on the roster of a match that has not finished.
    #[error("player is already in an active match")]
    AlreadyInMatch,
    /// The player is serving a timed queue ban.
    #[error("player is banned from queueing until {until}")]
    QueueBanned {
        /// When the ban lapses, as an RFC 3339 timestamp.
        until: String,
    },
    /// The queue requires a role the player does not hold.
    #[error("player does not have a required whitelist role")]
    MissingWhitelistRole,
    /// The player holds a role that is barred from this queue.
    #[error("player has a blacklisted role")]
    BlacklistedRole,
    /// The requested match state change is not one the state machine allows.
    #[error("invalid transition from {from} to {to}")]
    InvalidTransition {
        /// The state the match is in.
        from: MatchState,
        /// The state that was requested.
        to: MatchState,
    },
    /// The command requires a state the match is not in.
    #[error("expected match state {expected}, found {actual}")]
    UnexpectedState {
        /// The state the command requires.
        expected: MatchState,
        /// The state the match is actually in.
        actual: MatchState,
    },
    /// Somebody other than the captain on the clock tried to pick.
    #[error("only the active captain may pick")]
    NotActiveCaptain,
    /// The chosen player is not available to be picked.
    #[error("player {0} is not available to be picked")]
    PlayerNotInPool(UserId),
    /// Every roster place is already filled.
    #[error("the draft is already complete")]
    DraftComplete,
    /// That captain slot already has an occupant.
    #[error("team {0} already has a captain")]
    CaptainSlotTaken(usize),
    /// The team index does not exist in this match.
    #[error("team index {0} is out of range")]
    NoSuchTeam(usize),
    /// The player is not on the roster of this match.
    #[error("player is not part of this match")]
    NotInMatch,
    /// The queue has no maps configured to choose from.
    #[error("map pool is empty")]
    EmptyMapPool,
    /// A map vote must offer between 2 and 9 candidates.
    #[error("map vote requires between 2 and 9 candidates, got {0}")]
    InvalidVoteSize(usize),
    /// The draft pick order is malformed, or names a team that does not exist.
    #[error("invalid pick order {0:?}: expected letters A-Z only")]
    InvalidPickOrder(String),
    /// A configuration value is not usable, with a human-readable reason.
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),
    /// The match has already reached a terminal state.
    #[error("this match has already been finalized")]
    AlreadyFinalized,
}

/// Errors produced by application services.
#[derive(Debug, Error)]
pub enum ServiceError {
    /// A domain rule rejected the request.
    #[error(transparent)]
    Domain(#[from] DomainError),
    /// The database call failed.
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    /// PUGbot has not been enabled in this channel.
    #[error("this channel is not enabled for PUGbot")]
    ChannelNotEnabled,
    /// The channel is enabled but has no queue yet.
    #[error("this channel has no queue; an administrator must run /queue create")]
    NoQueue,
    /// The channel already owns a queue; a channel may have only one.
    #[error("this channel already has a queue")]
    QueueExists,
    /// No match matched the request.
    #[error("no match found")]
    NoMatch,
    /// Another handler changed the match first; the caller should retry.
    #[error("match {0} was modified concurrently; please retry")]
    Conflict(MatchId),
    /// The caller lacks the required permission level.
    #[error("you do not have permission to do that")]
    Forbidden,
    /// The player has never been rated in this channel.
    #[error("no rating data for that player in <#{0}>")]
    NoRatingData(ChannelId),
    /// The action is available only when running in debug mode.
    #[error("this action is only available in debug mode")]
    DebugOnly,
    /// A well-formed request that is not allowed right now, carrying a reason to show the caller.
    #[error("{0}")]
    Rejected(String),
    /// Any other failure, carried as an `anyhow` chain at the executable boundary.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl ServiceError {
    /// Whether this is the caller's mistake rather than a bot fault.
    ///
    /// User errors are shown to the caller verbatim and ephemerally; anything
    /// else is logged and replaced with a generic message, so an internal
    /// detail such as a connection string can never reach a channel.
    #[must_use]
    pub fn is_user_error(&self) -> bool {
        matches!(
            self,
            ServiceError::Domain(_)
                | ServiceError::ChannelNotEnabled
                | ServiceError::NoQueue
                | ServiceError::QueueExists
                | ServiceError::NoMatch
                | ServiceError::Forbidden
                | ServiceError::NoRatingData(_)
                | ServiceError::DebugOnly
                | ServiceError::Rejected(_)
        )
    }
}

/// The result type returned by every service method.
pub type ServiceResult<T> = Result<T, ServiceError>;
/// The result type returned by every fallible domain operation.
pub type DomainResult<T> = Result<T, DomainError>;
