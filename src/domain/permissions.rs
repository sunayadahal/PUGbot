//! Permission levels and the policy that maps a Discord member onto one.
//!
//! Both native Discord permissions and configured roles are honoured: a member
//! with the Discord "Manage Guild" permission is an administrator even if no
//! role has been configured, and a configured role grants the level even if
//! Discord would not.

use serde::{Deserialize, Serialize};

use crate::domain::ids::{RoleId, UserId};
use crate::domain::settings::ChannelSettings;

/// What a caller is allowed to do, in increasing order of authority.
///
/// The ordering is meaningful: a higher level implies every lower one, which
/// [`PermissionLevel::allows`] relies on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionLevel {
    /// An ordinary player: queue, play, report, and manage their own settings.
    Player = 0,
    /// Manages queue membership and live matches, issues queue bans.
    Moderator = 1,
    /// Configures channels, queues, ratings, and statistics.
    Administrator = 2,
    /// The configured bot owner. Outranks every guild-level role.
    Owner = 3,
}

impl PermissionLevel {
    /// A stable lowercase name, used in audit records and log fields.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            PermissionLevel::Player => "player",
            PermissionLevel::Moderator => "moderator",
            PermissionLevel::Administrator => "administrator",
            PermissionLevel::Owner => "owner",
        }
    }

    /// Whether this level satisfies a requirement of `required`.
    #[must_use]
    pub fn allows(self, required: PermissionLevel) -> bool {
        self >= required
    }
}

/// The caller of a command, reduced to what the policy needs.
#[derive(Debug, Clone)]
pub struct Actor {
    /// Who is calling.
    pub user: UserId,
    /// The roles they hold in the guild.
    pub roles: Vec<RoleId>,
    /// Discord's `Manage Guild` (or Administrator) permission.
    pub is_guild_admin: bool,
    /// Discord's `Manage Messages` permission, the closest native analogue of
    /// a PUG moderator.
    pub is_guild_moderator: bool,
    /// Configured bot owner.
    pub is_bot_owner: bool,
}

impl Actor {
    /// An actor with no elevated permissions, for tests and for callers whose
    /// member data is unavailable.
    #[must_use]
    pub fn player(user: UserId) -> Self {
        Self {
            user,
            roles: Vec::new(),
            is_guild_admin: false,
            is_guild_moderator: false,
            is_bot_owner: false,
        }
    }

    /// Resolves the actor's level in a channel.
    ///
    /// Native Discord permissions and configured roles both grant a level, and
    /// the highest applicable one wins. A member with Discord's *Manage Guild*
    /// permission is an administrator even where no admin role is configured,
    /// and a configured role grants its level even where Discord would not.
    pub fn level(&self, settings: &ChannelSettings) -> PermissionLevel {
        if self.is_bot_owner {
            return PermissionLevel::Owner;
        }
        let has_admin_role = settings
            .admin_role_id
            .is_some_and(|role| self.roles.contains(&role));
        if self.is_guild_admin || has_admin_role {
            return PermissionLevel::Administrator;
        }
        let has_moderator_role = settings
            .moderator_role_id
            .is_some_and(|role| self.roles.contains(&role));
        if self.is_guild_moderator || has_moderator_role {
            return PermissionLevel::Moderator;
        }
        PermissionLevel::Player
    }

    /// Whether this actor meets `required` in the given channel.
    #[must_use]
    pub fn can(&self, settings: &ChannelSettings, required: PermissionLevel) -> bool {
        self.level(settings).allows(required)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> ChannelSettings {
        ChannelSettings {
            admin_role_id: Some(RoleId(100)),
            moderator_role_id: Some(RoleId(200)),
            ..Default::default()
        }
    }

    #[test]
    fn a_plain_member_is_a_player() {
        let actor = Actor::player(UserId(1));
        assert_eq!(actor.level(&settings()), PermissionLevel::Player);
        assert!(!actor.can(&settings(), PermissionLevel::Moderator));
    }

    #[test]
    fn configured_roles_grant_their_level() {
        let mut actor = Actor::player(UserId(1));
        actor.roles = vec![RoleId(200)];
        assert_eq!(actor.level(&settings()), PermissionLevel::Moderator);
        actor.roles = vec![RoleId(100)];
        assert_eq!(actor.level(&settings()), PermissionLevel::Administrator);
    }

    #[test]
    fn native_discord_permissions_grant_their_level_without_a_configured_role() {
        let bare = ChannelSettings::default();
        let mut actor = Actor::player(UserId(1));
        actor.is_guild_moderator = true;
        assert_eq!(actor.level(&bare), PermissionLevel::Moderator);
        actor.is_guild_admin = true;
        assert_eq!(actor.level(&bare), PermissionLevel::Administrator);
    }

    #[test]
    fn the_bot_owner_outranks_everyone() {
        let mut actor = Actor::player(UserId(1));
        actor.is_bot_owner = true;
        assert_eq!(actor.level(&settings()), PermissionLevel::Owner);
        assert!(actor.can(&settings(), PermissionLevel::Administrator));
    }

    #[test]
    fn levels_are_ordered_and_inclusive() {
        assert!(PermissionLevel::Administrator.allows(PermissionLevel::Moderator));
        assert!(PermissionLevel::Moderator.allows(PermissionLevel::Moderator));
        assert!(!PermissionLevel::Moderator.allows(PermissionLevel::Administrator));
        assert!(PermissionLevel::Owner.allows(PermissionLevel::Player));
    }

    #[test]
    fn an_unrelated_role_grants_nothing() {
        let mut actor = Actor::player(UserId(1));
        actor.roles = vec![RoleId(999)];
        assert_eq!(actor.level(&settings()), PermissionLevel::Player);
    }
}
