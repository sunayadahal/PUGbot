//! The pure domain layer.
//!
//! Nothing in here performs I/O, talks to Discord, or reads a clock it was not
//! given. Every rule that decides whether an action is legal lives here so it
//! can be unit-tested exhaustively and reused by any adapter.

pub mod checkin;
pub mod clock;
pub mod draft;
pub mod ids;
pub mod maps;
pub mod match_state;
pub mod permissions;
pub mod queue;
pub mod rating;
pub mod report;
pub mod settings;
pub mod teams;
