//! Queue membership: join, leave, list, promote, and start.

use chrono::{DateTime, Utc};

use super::{Announcement, AppContext};
use crate::domain::ids::{ChannelId, MatchId, RoleId, UserId};
use crate::domain::permissions::{Actor, PermissionLevel};
use crate::domain::queue::{
    expiry_for, promotion_allowed, JoinRequest, QueueMember, QueueSnapshot,
};
use crate::error::{ServiceError, ServiceResult};
use crate::repositories::{ChannelConfigRow, QueueInsert, QueueRow};
use crate::services::match_svc::MatchService;

/// Queue membership: join, leave, list, promote, and start.
#[derive(Debug, Clone)]
pub struct QueueService {
    ctx: AppContext,
}

/// The result of a successful `/add`.
#[derive(Debug, Clone)]
pub struct JoinResult {
    /// The queue as it stands after the join.
    pub snapshot: QueueSnapshot,
    /// Set when the join filled the queue and started a match.
    pub started: Option<MatchId>,
    /// The player's configured join phrase, if they have one.
    pub phrase: Option<String>,
    /// When the new slot lapses, if ever.
    pub expires_at: Option<DateTime<Utc>>,
}

/// The channel's queue plus its configuration, resolved once per command.
#[derive(Debug, Clone)]
pub struct QueueContext {
    /// The channel's configuration.
    pub channel: ChannelConfigRow,
    /// The channel's single queue.
    pub queue: QueueRow,
}

impl QueueService {
    /// Wraps the shared application context.
    #[must_use]
    pub fn new(ctx: AppContext) -> Self {
        Self { ctx }
    }

    /// Resolves the channel's single queue, rejecting disabled channels.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::ChannelNotEnabled`], [`ServiceError::NoQueue`],
    /// [`ServiceError::Rejected`] if the guild is not allowed in this mode, or
    /// [`ServiceError::Database`].
    pub async fn context(&self, channel: ChannelId) -> ServiceResult<QueueContext> {
        let config = self.ctx.store.require_enabled_channel(channel).await?;
        self.ctx.ensure_guild_allowed(config.guild)?;
        let queue = self.ctx.store.require_queue(channel).await?;
        Ok(QueueContext {
            channel: config,
            queue,
        })
    }

    /// The queue's current membership.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`] if a query fails.
    pub async fn snapshot(&self, context: &QueueContext) -> ServiceResult<QueueSnapshot> {
        let members = self.ctx.store.queue_members(context.queue.id).await?;
        Ok(QueueSnapshot {
            size: context.queue.settings.size,
            members: members
                .into_iter()
                .map(|row| QueueMember {
                    user: row.user,
                    joined_at: row.joined_at,
                    expires_at: row.expires_at,
                })
                .collect(),
        })
    }

    /// `/add`: join this channel's queue.
    ///
    /// Every precondition is evaluated by the domain first, for a good error
    /// message; capacity and duplication are then settled atomically by the
    /// repository, so simultaneous joins cannot overfill the queue. The
    /// snapshot is re-read afterwards, so the reported count is the real one.
    ///
    /// Launches a match if the join fills a queue with autostart enabled.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Domain`] carrying the failing precondition —
    /// already queued, banned, already in a match, missing or blacklisted
    /// role, or queue full — or [`ServiceError::Database`].
    pub async fn add(
        &self,
        context: &QueueContext,
        user: UserId,
        member_roles: &[RoleId],
        expiry_override_seconds: Option<i64>,
    ) -> ServiceResult<JoinResult> {
        let now = self.ctx.now();
        let snapshot = self.snapshot(context).await?;
        let prefs = self.ctx.store.user_prefs(user).await?;
        let ban_expires_at = self
            .ctx
            .store
            .active_ban_until(context.channel.guild, user, now)
            .await?;
        let live_match = MatchService::new(self.ctx.clone())
            .live_match_for(
                user,
                context.channel.channel,
                &context.channel.settings,
                context.channel.guild,
            )
            .await?;

        JoinRequest {
            settings: &context.queue.settings,
            member_roles,
            ban_expires_at,
            already_queued: snapshot.contains(user),
            in_live_match: live_match.is_some(),
            queue_len: snapshot.len(),
            now,
        }
        .evaluate()?;

        let expires_at = expiry_for(
            now,
            expiry_override_seconds,
            prefs.default_expiry_seconds,
            &context.channel.settings,
        );
        // The checks above give good error messages, but they read a snapshot
        // that another join can invalidate. The insert below is the authority
        // on duplicates and capacity, and it is atomic.
        match self
            .ctx
            .store
            .add_queue_member_atomic(
                context.queue.id,
                context.queue.settings.size,
                user,
                now,
                expires_at,
            )
            .await?
        {
            QueueInsert::Added { .. } => {}
            QueueInsert::Duplicate => return Err(crate::error::DomainError::AlreadyQueued.into()),
            QueueInsert::Full => return Err(crate::error::DomainError::QueueFull.into()),
        }

        let snapshot = self.snapshot(context).await?;
        let phrase = self
            .ctx
            .store
            .random_phrase(context.channel.channel, user)
            .await?;

        let started = if snapshot.should_autostart(&context.queue.settings) {
            self.start_match(context, None).await?
        } else {
            None
        };

        Ok(JoinResult {
            snapshot,
            started,
            phrase,
            expires_at,
        })
    }

    /// `/remove`: leave this channel's queue, returning the queue afterwards.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::NotQueued`](crate::error::DomainError::NotQueued)
    /// if the player held no slot, or [`ServiceError::Database`].
    pub async fn remove(
        &self,
        context: &QueueContext,
        user: UserId,
    ) -> ServiceResult<QueueSnapshot> {
        let removed = self
            .ctx
            .store
            .remove_queue_member(context.queue.id, user)
            .await?;
        if !removed {
            return Err(crate::error::DomainError::NotQueued.into());
        }
        self.snapshot(context).await
    }

    /// Moderator: add a player, bypassing role checks but not bans or the
    /// one-live-match rule.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Forbidden`] unless the caller is a moderator,
    /// otherwise as [`QueueService::add`].
    pub async fn force_add(
        &self,
        context: &QueueContext,
        actor: &Actor,
        user: UserId,
    ) -> ServiceResult<JoinResult> {
        self.ctx.require_permission(
            actor,
            &context.channel.settings,
            PermissionLevel::Moderator,
        )?;
        let result = self.add(context, user, &[], None).await;
        // A whitelist or blacklist role must not stop a moderator; retry
        // without the role checks if that was the only obstacle.
        let result = match result {
            Err(ServiceError::Domain(
                crate::error::DomainError::MissingWhitelistRole
                | crate::error::DomainError::BlacklistedRole,
            )) => {
                let mut relaxed = context.clone();
                relaxed.queue.settings.whitelist_role_id = None;
                relaxed.queue.settings.blacklist_role_id = None;
                self.add(&relaxed, user, &[], None).await
            }
            other => other,
        }?;
        self.ctx
            .audit(
                Some(context.channel.guild),
                Some(context.channel.channel),
                Some(actor.user),
                "queue.force_add",
                Some(&user.to_string()),
                serde_json::json!({}),
            )
            .await;
        Ok(result)
    }

    /// Moderator: remove a player from the queue.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Forbidden`] unless the caller is a moderator,
    /// otherwise as [`QueueService::remove`].
    pub async fn force_remove(
        &self,
        context: &QueueContext,
        actor: &Actor,
        user: UserId,
    ) -> ServiceResult<QueueSnapshot> {
        self.ctx.require_permission(
            actor,
            &context.channel.settings,
            PermissionLevel::Moderator,
        )?;
        let snapshot = self.remove(context, user).await?;
        self.ctx
            .audit(
                Some(context.channel.guild),
                Some(context.channel.channel),
                Some(actor.user),
                "queue.force_remove",
                Some(&user.to_string()),
                serde_json::json!({}),
            )
            .await;
        Ok(snapshot)
    }

    /// Empties the queue. Returns how many slots were removed.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Forbidden`] unless the caller is a moderator,
    /// or [`ServiceError::Database`].
    pub async fn clear(&self, context: &QueueContext, actor: &Actor) -> ServiceResult<u64> {
        self.ctx.require_permission(
            actor,
            &context.channel.settings,
            PermissionLevel::Moderator,
        )?;
        let removed = self.ctx.store.clear_queue(context.queue.id).await?;
        self.ctx
            .audit(
                Some(context.channel.guild),
                Some(context.channel.channel),
                Some(actor.user),
                "queue.cleared",
                None,
                serde_json::json!({ "removed": removed }),
            )
            .await;
        Ok(removed)
    }

    /// Starts a match from the current queue.
    ///
    /// With `actor` set this is a manual `/queue start` and needs moderator
    /// rights; without it, this is the autostart path. A manual start on a
    /// partially full queue trims the roster to a size the configured teams
    /// divide evenly.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Forbidden`] if a non-moderator asks,
    /// [`ServiceError::Rejected`] if too few players are queued, or
    /// [`ServiceError::Database`].
    pub async fn start_match(
        &self,
        context: &QueueContext,
        actor: Option<&Actor>,
    ) -> ServiceResult<Option<MatchId>> {
        if let Some(actor) = actor {
            self.ctx.require_permission(
                actor,
                &context.channel.settings,
                PermissionLevel::Moderator,
            )?;
        }
        let snapshot = self.snapshot(context).await?;
        if snapshot.len() < 2 {
            return Err(ServiceError::Rejected(
                "at least two players are needed to start".to_string(),
            ));
        }

        // A manual start on a partially full queue plays with who is there, so
        // the roster is trimmed to a size the configured teams can divide.
        let mut roster = snapshot.roster();
        let team_count = context.queue.settings.team_count as usize;
        if context.queue.settings.uses_teams() && team_count > 0 {
            let usable = roster.len() - (roster.len() % team_count);
            if usable < team_count {
                return Err(ServiceError::Rejected(format!(
                    "need at least {team_count} players to fill {team_count} teams"
                )));
            }
            roster.truncate(usable);
        }
        roster.truncate(context.queue.settings.size as usize);

        let id = MatchService::new(self.ctx.clone())
            .launch(&context.channel, &context.queue, roster)
            .await?;
        Ok(Some(id))
    }

    /// `/promote`: ping the promotion role, subject to cooldown.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Rejected`] if the cooldown has not elapsed or
    /// the queue is already full, or [`ServiceError::Database`].
    pub async fn promote(&self, context: &QueueContext) -> ServiceResult<PromoteResult> {
        let now = self.ctx.now();
        promotion_allowed(
            context.queue.last_promoted_at,
            context.queue.settings.promotion_cooldown_seconds,
            now,
        )
        .map_err(|remaining| {
            ServiceError::Rejected(format!(
                "this queue was promoted recently; try again in {}s",
                remaining.num_seconds().max(1)
            ))
        })?;

        let snapshot = self.snapshot(context).await?;
        if snapshot.is_full() {
            return Err(ServiceError::Rejected(
                "the queue is already full".to_string(),
            ));
        }
        self.ctx.store.mark_promoted(context.queue.id, now).await?;
        Ok(PromoteResult {
            needed: snapshot.slots_remaining(),
            role: context.queue.settings.promotion_role_id,
        })
    }

    /// Removes expired players across every queue and tells them why. Returns
    /// how many slots were released.
    ///
    /// Safe to run repeatedly and after a restart: the delete is the only state
    /// change, and it only ever matches rows already past due.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`] if a query fails.
    pub async fn sweep_expired(&self) -> ServiceResult<usize> {
        let now = self.ctx.now();
        let removed = self.ctx.store.remove_expired_members(now).await?;
        for (_, user) in &removed {
            self.ctx
                .notifier
                .direct_message(
                    *user,
                    "You were removed from a queue because your time ran out.".to_string(),
                )
                .await;
        }
        Ok(removed.len())
    }

    /// Removes a player from every queue they sit in, for presence handling and
    /// queue bans. Returns how many slots were released.
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`] if a query fails.
    pub async fn remove_everywhere(&self, user: UserId) -> ServiceResult<usize> {
        let queues = self.ctx.store.queues_for_member(user).await?;
        let mut removed = 0;
        for queue in queues {
            if self.ctx.store.remove_queue_member(queue, user).await? {
                removed += 1;
            }
        }
        Ok(removed)
    }

    /// Says something in the channel through the notifier.
    pub async fn announce(&self, channel: ChannelId, text: String) {
        self.ctx
            .notifier
            .announce(channel, Announcement::Text(text))
            .await;
    }
}

/// What `/promote` should announce.
#[derive(Debug, Clone, Copy)]
pub struct PromoteResult {
    /// How many more players the queue needs.
    pub needed: usize,
    /// The role to mention, if one is configured.
    pub role: Option<RoleId>,
}

/// Renders the queue list for `/who`, mentioning each player in join order.
/// Returns an empty string for an empty queue.
pub fn render_queue(snapshot: &QueueSnapshot) -> String {
    if snapshot.is_empty() {
        return String::new();
    }
    snapshot
        .members
        .iter()
        .map(|member| format!("<@{}>", member.user))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(users: &[i64]) -> QueueSnapshot {
        let now = Utc::now();
        QueueSnapshot {
            size: 10,
            members: users
                .iter()
                .map(|id| QueueMember {
                    user: UserId(*id),
                    joined_at: now,
                    expires_at: None,
                })
                .collect(),
        }
    }

    #[test]
    fn an_empty_queue_renders_as_nothing() {
        assert_eq!(render_queue(&snapshot(&[])), "");
    }

    #[test]
    fn queued_players_render_as_mentions_in_join_order() {
        assert_eq!(render_queue(&snapshot(&[7, 3])), "<@7>, <@3>");
    }
}
