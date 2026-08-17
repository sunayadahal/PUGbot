//! Queue membership rules: who may join, when they expire, and when the queue
//! is ready to launch.

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::domain::ids::{RoleId, UserId};
use crate::domain::settings::{ChannelSettings, QueueSettings};
use crate::error::{DomainError, DomainResult};

/// One player's place in a queue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueMember {
    /// Who is queued.
    pub user: UserId,
    /// When they joined. Also the ordering key for the roster.
    pub joined_at: DateTime<Utc>,
    /// When their slot lapses. `None` means it never does.
    pub expires_at: Option<DateTime<Utc>>,
}

impl QueueMember {
    /// Whether this slot has lapsed and should be swept.
    #[must_use]
    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        self.expires_at.is_some_and(|at| at <= now)
    }
}

/// Everything the domain needs to decide whether one player may join.
#[derive(Debug, Clone)]
pub struct JoinRequest<'a> {
    /// The target queue's configuration.
    pub settings: &'a QueueSettings,
    /// The roles the joining member holds, for whitelist and blacklist checks.
    pub member_roles: &'a [RoleId],
    /// End of an active queue ban, if any.
    pub ban_expires_at: Option<DateTime<Utc>>,
    /// Whether the player already holds a slot in this queue.
    pub already_queued: bool,
    /// The player is on the roster of a match that has not finished, within
    /// the channel's configured scope.
    pub in_live_match: bool,
    /// How many players are currently queued.
    pub queue_len: usize,
    /// The current time, supplied rather than read.
    pub now: DateTime<Utc>,
}

impl JoinRequest<'_> {
    /// Runs every join precondition in a fixed order so the player always sees
    /// the most important reason first.
    ///
    /// The order is: already queued, banned, already in a match, blacklisted,
    /// not whitelisted, queue full. A blacklist therefore beats a whitelist.
    ///
    /// # Errors
    ///
    /// Returns the [`DomainError`] describing the first failing precondition.
    ///
    /// # Note
    ///
    /// This reads a snapshot that a concurrent join can invalidate. It exists
    /// to produce a good message; capacity and duplication are settled
    /// atomically by
    /// [`crate::repositories::Store::add_queue_member_atomic`].
    pub fn evaluate(&self) -> DomainResult<()> {
        if self.already_queued {
            return Err(DomainError::AlreadyQueued);
        }
        if let Some(until) = self.ban_expires_at {
            if until > self.now {
                return Err(DomainError::QueueBanned {
                    until: until.to_rfc3339(),
                });
            }
        }
        if self.in_live_match {
            return Err(DomainError::AlreadyInMatch);
        }
        if let Some(blacklist) = self.settings.blacklist_role_id {
            if self.member_roles.contains(&blacklist) {
                return Err(DomainError::BlacklistedRole);
            }
        }
        if let Some(whitelist) = self.settings.whitelist_role_id {
            if !self.member_roles.contains(&whitelist) {
                return Err(DomainError::MissingWhitelistRole);
            }
        }
        if self.queue_len >= self.settings.size as usize {
            return Err(DomainError::QueueFull);
        }
        Ok(())
    }
}

/// Computes when a joining player's slot expires.
///
/// Precedence is: this-session override, then the player's own default, then
/// the channel default. Every value is clamped to the channel maximum, and an
/// explicit zero means "never expire" where the channel allows it.
pub fn expiry_for(
    now: DateTime<Utc>,
    session_override_seconds: Option<i64>,
    player_default_seconds: Option<i64>,
    channel: &ChannelSettings,
) -> Option<DateTime<Utc>> {
    let requested = session_override_seconds
        .or(player_default_seconds)
        .unwrap_or(channel.default_expiry_seconds);
    if requested <= 0 {
        return None;
    }
    Some(now + Duration::seconds(channel.clamp_expiry(requested)))
}

/// Whether a player's armed auto-ready is still valid.
pub fn auto_ready_active(auto_ready_until: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
    auto_ready_until.is_some_and(|until| until > now)
}

/// Whether presence-based removal applies to a player right now.
pub fn should_remove_for_presence(
    channel: &ChannelSettings,
    is_offline: bool,
    is_afk: bool,
    allow_offline_until: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> bool {
    let opted_out =
        channel.allow_offline_opt_out && allow_offline_until.is_some_and(|until| until > now);
    if opted_out {
        return false;
    }
    (channel.remove_offline && is_offline) || (channel.remove_afk && is_afk)
}

/// The queue as the launch check sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct QueueSnapshot {
    /// The queued players, in join order.
    pub members: Vec<QueueMember>,
    /// How many players the queue needs to launch a match.
    pub size: u32,
}

impl QueueSnapshot {
    /// How many players are queued.
    #[must_use]
    pub fn len(&self) -> usize {
        self.members.len()
    }

    /// Whether nobody is queued.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.members.is_empty()
    }

    /// Free slots before the queue is full, floored at zero.
    #[must_use]
    pub fn slots_remaining(&self) -> usize {
        (self.size as usize).saturating_sub(self.members.len())
    }

    /// Whether the queue has reached its configured size.
    #[must_use]
    pub fn is_full(&self) -> bool {
        self.members.len() >= self.size as usize
    }

    /// Whether this player already holds a slot.
    #[must_use]
    pub fn contains(&self, user: UserId) -> bool {
        self.members.iter().any(|m| m.user == user)
    }

    /// Members whose expiry has passed, in join order.
    pub fn expired(&self, now: DateTime<Utc>) -> Vec<UserId> {
        self.members
            .iter()
            .filter(|m| m.is_expired(now))
            .map(|m| m.user)
            .collect()
    }

    /// Whether the queue should launch on its own.
    pub fn should_autostart(&self, settings: &QueueSettings) -> bool {
        settings.autostart && self.is_full()
    }

    /// The roster in join order, which is the order a match is built from.
    pub fn roster(&self) -> Vec<UserId> {
        self.members.iter().map(|m| m.user).collect()
    }
}

/// Whether a promotion is allowed, given when the last one happened.
///
/// # Errors
///
/// Returns the remaining wait as an `Err` when the cooldown has not elapsed, so
/// the caller can tell the player exactly how long to wait.
pub fn promotion_allowed(
    last_promoted_at: Option<DateTime<Utc>>,
    cooldown_seconds: i64,
    now: DateTime<Utc>,
) -> Result<(), Duration> {
    match last_promoted_at {
        Some(last) => {
            let ready_at = last + Duration::seconds(cooldown_seconds);
            if ready_at <= now {
                Ok(())
            } else {
                Err(ready_at - now)
            }
        }
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-01-01T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn request<'a>(settings: &'a QueueSettings, roles: &'a [RoleId]) -> JoinRequest<'a> {
        JoinRequest {
            settings,
            member_roles: roles,
            ban_expires_at: None,
            already_queued: false,
            in_live_match: false,
            queue_len: 0,
            now: now(),
        }
    }

    #[test]
    fn a_clean_player_may_join() {
        let settings = QueueSettings::default();
        request(&settings, &[]).evaluate().unwrap();
    }

    #[test]
    fn duplicate_membership_is_refused_before_anything_else() {
        let settings = QueueSettings {
            whitelist_role_id: Some(RoleId(9)),
            ..Default::default()
        };
        let mut req = request(&settings, &[]);
        req.already_queued = true;
        req.in_live_match = true;
        assert_eq!(req.evaluate().unwrap_err(), DomainError::AlreadyQueued);
    }

    #[test]
    fn an_active_ban_blocks_joining_but_an_expired_one_does_not() {
        let settings = QueueSettings::default();
        let mut req = request(&settings, &[]);
        req.ban_expires_at = Some(now() + Duration::hours(1));
        assert!(matches!(
            req.evaluate().unwrap_err(),
            DomainError::QueueBanned { .. }
        ));
        req.ban_expires_at = Some(now() - Duration::seconds(1));
        req.evaluate().unwrap();
    }

    #[test]
    fn a_live_match_blocks_joining() {
        let settings = QueueSettings::default();
        let mut req = request(&settings, &[]);
        req.in_live_match = true;
        assert_eq!(req.evaluate().unwrap_err(), DomainError::AlreadyInMatch);
    }

    #[test]
    fn access_roles_are_enforced() {
        let settings = QueueSettings {
            whitelist_role_id: Some(RoleId(10)),
            blacklist_role_id: Some(RoleId(20)),
            ..Default::default()
        };
        assert_eq!(
            request(&settings, &[]).evaluate().unwrap_err(),
            DomainError::MissingWhitelistRole
        );
        assert_eq!(
            request(&settings, &[RoleId(10), RoleId(20)])
                .evaluate()
                .unwrap_err(),
            DomainError::BlacklistedRole,
            "a blacklist beats a whitelist"
        );
        request(&settings, &[RoleId(10)]).evaluate().unwrap();
    }

    #[test]
    fn a_full_queue_is_refused() {
        let settings = QueueSettings {
            size: 4,
            ..Default::default()
        };
        let mut req = request(&settings, &[]);
        req.queue_len = 4;
        assert_eq!(req.evaluate().unwrap_err(), DomainError::QueueFull);
        req.queue_len = 3;
        req.evaluate().unwrap();
    }

    #[test]
    fn expiry_precedence_is_session_then_player_then_channel() {
        let channel = ChannelSettings::default();
        let base = now();
        assert_eq!(
            expiry_for(base, Some(600), Some(1200), &channel),
            Some(base + Duration::seconds(600))
        );
        assert_eq!(
            expiry_for(base, None, Some(1200), &channel),
            Some(base + Duration::seconds(1200))
        );
        assert_eq!(
            expiry_for(base, None, None, &channel),
            Some(base + Duration::seconds(channel.default_expiry_seconds))
        );
    }

    #[test]
    fn expiry_is_capped_by_the_channel_maximum() {
        let channel = ChannelSettings::default();
        let base = now();
        assert_eq!(
            expiry_for(base, Some(999_999), None, &channel),
            Some(base + Duration::seconds(channel.max_expiry_seconds))
        );
    }

    #[test]
    fn zero_expiry_means_never() {
        let channel = ChannelSettings::default();
        assert_eq!(expiry_for(now(), Some(0), None, &channel), None);
    }

    #[test]
    fn expired_members_are_listed_in_join_order() {
        let base = now();
        let snapshot = QueueSnapshot {
            size: 10,
            members: vec![
                QueueMember {
                    user: UserId(1),
                    joined_at: base,
                    expires_at: Some(base - Duration::seconds(1)),
                },
                QueueMember {
                    user: UserId(2),
                    joined_at: base,
                    expires_at: None,
                },
                QueueMember {
                    user: UserId(3),
                    joined_at: base,
                    expires_at: Some(base + Duration::hours(1)),
                },
            ],
        };
        assert_eq!(snapshot.expired(base), vec![UserId(1)]);
        assert_eq!(snapshot.slots_remaining(), 7);
        assert!(!snapshot.is_full());
        assert!(snapshot.contains(UserId(2)));
    }

    #[test]
    fn autostart_only_fires_on_a_full_queue_with_the_setting_on() {
        let mut settings = QueueSettings {
            size: 2,
            ..Default::default()
        };
        let base = now();
        let member = |id| QueueMember {
            user: UserId(id),
            joined_at: base,
            expires_at: None,
        };
        let full = QueueSnapshot {
            members: vec![member(1), member(2)],
            size: 2,
        };
        assert!(full.should_autostart(&settings));
        settings.autostart = false;
        assert!(!full.should_autostart(&settings));
        settings.autostart = true;
        let partial = QueueSnapshot {
            members: vec![member(1)],
            size: 2,
        };
        assert!(!partial.should_autostart(&settings));
    }

    #[test]
    fn presence_removal_respects_the_opt_out() {
        let channel = ChannelSettings {
            remove_offline: true,
            ..Default::default()
        };
        let base = now();
        assert!(should_remove_for_presence(
            &channel, true, false, None, base
        ));
        assert!(!should_remove_for_presence(
            &channel,
            true,
            false,
            Some(base + Duration::hours(1)),
            base
        ));
        assert!(should_remove_for_presence(
            &channel,
            true,
            false,
            Some(base - Duration::seconds(1)),
            base
        ));
    }

    #[test]
    fn presence_removal_ignores_the_opt_out_when_the_channel_forbids_it() {
        let channel = ChannelSettings {
            remove_offline: true,
            allow_offline_opt_out: false,
            ..Default::default()
        };
        let base = now();
        assert!(should_remove_for_presence(
            &channel,
            true,
            false,
            Some(base + Duration::hours(1)),
            base
        ));
    }

    #[test]
    fn afk_removal_is_configured_separately_from_offline_removal() {
        let channel = ChannelSettings {
            remove_afk: true,
            ..Default::default()
        };
        let base = now();
        assert!(should_remove_for_presence(
            &channel, false, true, None, base
        ));
        assert!(
            !should_remove_for_presence(&channel, true, false, None, base),
            "offline removal is off"
        );
    }

    #[test]
    fn promotion_cooldown_reports_the_remaining_wait() {
        let base = now();
        assert!(promotion_allowed(None, 600, base).is_ok());
        assert!(promotion_allowed(Some(base - Duration::seconds(601)), 600, base).is_ok());
        let wait = promotion_allowed(Some(base - Duration::seconds(60)), 600, base).unwrap_err();
        assert_eq!(wait, Duration::seconds(540));
    }

    #[test]
    fn auto_ready_expires() {
        let base = now();
        assert!(auto_ready_active(Some(base + Duration::minutes(1)), base));
        assert!(!auto_ready_active(Some(base - Duration::minutes(1)), base));
        assert!(!auto_ready_active(None, base));
    }
}
