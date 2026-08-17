#![deny(missing_docs)]
#![deny(missing_debug_implementations)]
#![deny(rustdoc::broken_intra_doc_links)]
#![deny(rustdoc::private_intra_doc_links)]
#![warn(rustdoc::missing_crate_level_docs)]
#![warn(clippy::missing_panics_doc)]
#![warn(clippy::missing_errors_doc)]

//! PUGbot — a Discord bot for organising pickup games (PUGs).
//!
//! Each PUG-enabled Discord text channel owns exactly one queue. The bot takes
//! that queue through check-in, team formation, map selection, play, result
//! reporting, and rating updates, and persists everything so a restart resumes
//! rather than losing state.
//!
//! # Architecture
//!
//! The crate is layered so that rules can be tested without Discord or a
//! database. Dependencies point downward only:
//!
//! ```text
//! discord      adapters: slash commands, components, embeds, gateway events
//!    │
//! services     use cases, transaction boundaries, permission enforcement
//!    │
//! domain       pure rules, state machines, invariants — no I/O
//!    │
//! repositories PostgreSQL persistence, schema-enforced invariants
//! ```
//!
//! * [`domain`] performs no I/O, reads no clock it was not given, and contains
//!   no Discord types. Every rule in it is unit-testable in isolation.
//! * [`services`] owns transactions and permission checks. No transaction is
//!   ever held across a Discord network call.
//! * [`repositories`] maps domain values onto PostgreSQL, and relies on schema
//!   constraints — not only service code — for the critical invariants.
//! * [`discord`] translates interactions into service calls and renders the
//!   results. It contains no rules of its own.
//!
//! Supporting modules: [`config`] (mode-separated configuration), [`jobs`]
//! (background sweeps), [`localization`] (message catalogs), and
//! [`observability`] (logging, metrics, health).
//!
//! # The match lifecycle
//!
//! ```text
//! QUEUED → CHECK_IN → TEAM_FORMATION → MAP_VOTE → ACTIVE → REPORT_PENDING → COMPLETED
//!                                                                         ↘ CANCELLED
//!                                                                         ↘ EXPIRED
//! ```
//!
//! Every transition is declared in [`domain::match_state::MatchState`] and
//! applied through a single function,
//! [`services::match_svc::MatchService::advance`], which drives a match as far
//! forward as the current facts allow and then stops. Commands change a fact —
//! a ready press, a pick, a vote — and call `advance`; the timer job calls it
//! too. Because each step re-reads the match and re-checks its preconditions, a
//! double-clicked button, a retried job, and a process restart are all no-ops
//! rather than corruption.
//!
//! # Invariants enforced by the database
//!
//! These are schema constraints, not merely service-layer checks:
//!
//! | Invariant | Enforcement |
//! | --- | --- |
//! | One queue per channel | `UNIQUE (channel_id)` on `queues` |
//! | One live match per player per channel | partial unique index on `match_players (channel_id, user_id) WHERE live` |
//! | Ratings applied exactly once per match | partial unique index on `rating_history (match_id, user_id)`, plus a one-shot `matches.rated` flag |
//! | Queue capacity under concurrency | row lock on the queue during [`repositories::Store::add_queue_member_atomic`] |
//!
//! State transitions additionally use optimistic locking on `matches.version`,
//! so a stale transition is rejected rather than overwriting a newer one.
//!
//! # Operating modes
//!
//! The process runs in exactly one of two modes, chosen by an explicit
//! command-line argument with no default. See [`config::Mode`]. Debug and
//! production read entirely separate environment variables and never fall back
//! to one another's values.
//!
//! # Example
//!
//! Domain rules are pure values, so they can be exercised directly:
//!
//! ```
//! use pugbot::domain::match_state::MatchState;
//!
//! // Declared transitions are the only legal ones.
//! assert!(MatchState::Active.can_transition_to(MatchState::Completed));
//!
//! // A finished match cannot be reopened without an audited correction.
//! assert!(!MatchState::Completed.can_transition_to(MatchState::Active));
//! ```
//!
//! # Documentation set
//!
//! This API reference is one item in a wider document set; see `docs/` in the
//! repository for the architecture description, administrator and player
//! guides, the operations runbook, and the requirements traceability matrix.

pub mod config;
pub mod discord;
pub mod domain;
pub mod error;
pub mod jobs;
pub mod localization;
pub mod observability;
pub mod repositories;
pub mod services;
