//! Newtype wrappers for Discord snowflakes and internal entity identifiers.
//!
//! Every identifier that crosses a boundary is a distinct type. A [`ChannelId`]
//! cannot be passed where a [`UserId`] is expected, which matters because both
//! are 64-bit integers and confusing them would compile silently otherwise.
//!
//! # Representation
//!
//! Discord IDs are stored as `i64` because PostgreSQL has no unsigned 64-bit
//! integer type. Snowflakes are 64-bit values whose top bit is a timestamp
//! field that will not be set for centuries, so they fit in a positive `i64`,
//! and the conversion is round-trip safe in both directions.
//!
//! # Example
//!
//! ```
//! use pugbot::domain::ids::UserId;
//!
//! let raw: u64 = 1_180_442_913_742_868_501;
//! let id = UserId::from(raw);
//! assert_eq!(id.as_u64(), raw);
//! assert_eq!(id.to_string(), raw.to_string());
//! ```

use std::fmt;

use serde::{Deserialize, Serialize};

/// Defines a newtype over a Discord snowflake.
///
/// Generates the identity accessors, `Display`, and conversions from both the
/// `u64` that Discord uses and the `i64` that PostgreSQL stores.
macro_rules! discord_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(
            /// The raw snowflake, as stored in the database.
            pub i64,
        );

        impl $name {
            /// The raw value, in the form the database uses.
            #[must_use]
            pub const fn get(self) -> i64 {
                self.0
            }

            /// The raw value, in the form the Discord API uses.
            #[must_use]
            pub const fn as_u64(self) -> u64 {
                self.0 as u64
            }
        }

        impl fmt::Display for $name {
            /// Writes the bare numeric ID, suitable for building a Discord
            /// mention such as `<@{id}>`.
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }

        impl From<u64> for $name {
            /// Converts from the Discord API representation.
            fn from(value: u64) -> Self {
                Self(value as i64)
            }
        }

        impl From<i64> for $name {
            /// Converts from the database representation.
            fn from(value: i64) -> Self {
                Self(value)
            }
        }
    };
}

discord_id!(
    /// A Discord guild, called a "server" in the user interface.
    ///
    /// Guilds are the scope for queue bans and, by default, the scope in which
    /// a live match blocks a player from queueing elsewhere.
    GuildId
);
discord_id!(
    /// A Discord text channel.
    ///
    /// This is also the queue boundary: an enabled channel owns at most one
    /// queue, enforced by a unique constraint. Queue commands issued in a
    /// channel always target that channel's queue and never name a queue.
    ChannelId
);
discord_id!(
    /// A Discord role.
    ///
    /// Used for access control (whitelist and blacklist), promotion pings,
    /// captain preference, and rank roles.
    RoleId
);
discord_id!(
    /// A Discord user.
    UserId
);
discord_id!(
    /// A Discord message.
    MessageId
);

/// Defines a newtype over a database primary key.
///
/// Unlike [`discord_id`] these have no `u64` form, because they never travel
/// through the Discord API as identifiers.
macro_rules! entity_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(
            /// The primary key value.
            pub i64,
        );

        impl $name {
            /// The raw primary key.
            #[must_use]
            pub const fn get(self) -> i64 {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }

        impl From<i64> for $name {
            fn from(value: i64) -> Self {
                Self(value)
            }
        }
    };
}

entity_id!(
    /// Primary key of a queue row.
    ///
    /// There is exactly one per enabled channel, so this is an implementation
    /// detail: players address a queue by posting in its channel.
    QueueId
);
entity_id!(
    /// Primary key of a match row.
    ///
    /// Unlike [`QueueId`] this is user-visible: it is the match number players
    /// see in announcements and pass to `/match` commands.
    MatchId
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snowflake_round_trips_through_i64() {
        let raw: u64 = 1_180_442_913_742_868_501;
        let id = UserId::from(raw);
        assert_eq!(id.as_u64(), raw);
    }

    #[test]
    fn display_matches_inner_value() {
        assert_eq!(GuildId(42).to_string(), "42");
        assert_eq!(MatchId(7).to_string(), "7");
    }
}
