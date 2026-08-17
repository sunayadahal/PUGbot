//! Channel enablement and queue configuration.
//!
//! Settings are edited through explicit patch structs rather than free-form
//! key/value strings: an unknown field is a compile error instead of a silent
//! no-op, and every change is validated before it is written.

use super::AppContext;
use crate::domain::draft::PickOrder;
use crate::domain::ids::{ChannelId, GuildId, RoleId};
use crate::domain::permissions::{Actor, PermissionLevel};
use crate::domain::rating::RatingSystemKind;
use crate::domain::settings::{
    CaptainMode, ChannelSettings, CheckInReturnPolicy, QueueScope, QueueSettings, RankTier,
    TeamFormationMode, TieBreak,
};
use crate::error::{ServiceError, ServiceResult};
use crate::localization::locale_installed;
use crate::repositories::{ChannelConfigRow, QueueRow};

/// Channel enablement and queue configuration.
#[derive(Debug, Clone)]
pub struct ConfigService {
    ctx: AppContext,
}

/// Fields a `/queue set` invocation may change. `None` means "leave alone".
/// Fields a `/queue set-*` invocation may change.
///
/// `None` means "leave this alone", which is what lets four themed subcommands
/// edit one settings object without clobbering each other. A role field is
/// doubly optional: `Some(None)` clears it, `None` leaves it unchanged.
#[derive(Debug, Default, Clone)]
pub struct QueuePatch {
    /// Display name for the queue.
    pub name: Option<String>,
    /// Free text shown by `/help`.
    pub description: Option<String>,
    /// How many players launch a match.
    pub size: Option<u32>,
    /// How many teams the roster is split into.
    pub team_count: Option<u32>,
    /// Whether a full queue launches on its own.
    pub autostart: Option<bool>,
    /// Whether results move ratings.
    pub ranked: Option<bool>,
    /// How teams are built.
    pub team_formation: Option<TeamFormationMode>,
    /// How captains are chosen.
    pub captain_mode: Option<CaptainMode>,
    /// Draft pattern such as `ABBA`; validated on apply.
    pub pick_order: Option<String>,
    /// Display name per team.
    pub team_names: Option<Vec<String>>,
    /// Display emoji per team.
    pub team_emojis: Option<Vec<String>>,
    /// Zero disables check-in entirely.
    pub check_in_seconds: Option<i64>,
    /// Whether one decline aborts immediately.
    pub check_in_abort_on_decline: Option<bool>,
    /// Who returns to the queue when a check-in fails.
    pub check_in_return_policy: Option<CheckInReturnPolicy>,
    /// The map pool.
    pub maps: Option<Vec<String>>,
    /// How many maps a match plays.
    pub map_pick_count: Option<u32>,
    /// How many recent matches to avoid repeating maps from.
    pub map_cooldown: Option<u32>,
    /// Zero disables the map vote.
    pub map_vote_candidates: Option<u32>,
    /// How a tied vote is resolved.
    pub map_vote_tie_break: Option<TieBreak>,
    /// Seconds before an unreported match expires.
    pub match_lifetime_seconds: Option<i64>,
    /// Server details shown at match start.
    pub server_text: Option<String>,
    /// Extra text shown at match start.
    pub start_message: Option<String>,
    /// `Some(None)` clears the role; `None` leaves it unchanged.
    pub promotion_role: Option<Option<RoleId>>,
    /// Role required to join.
    pub whitelist_role: Option<Option<RoleId>>,
    /// Role that blocks joining.
    pub blacklist_role: Option<Option<RoleId>>,
    /// Role preferred when picking captains.
    pub captain_role: Option<Option<RoleId>>,
    /// Minimum gap between promotions.
    pub promotion_cooldown_seconds: Option<i64>,
    /// Whether to send match-start direct messages.
    pub start_dm: Option<bool>,
    /// Whether to list streaming players.
    pub show_streams: Option<bool>,
}

impl QueuePatch {
    /// Whether the patch changes nothing.
    ///
    /// A no-op `/queue set` is a user mistake worth reporting rather than a
    /// silent success, so the service rejects it.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        // A patch with nothing set is a user mistake worth reporting rather
        // than a silent success.
        let QueuePatch {
            name,
            description,
            size,
            team_count,
            autostart,
            ranked,
            team_formation,
            captain_mode,
            pick_order,
            team_names,
            team_emojis,
            check_in_seconds,
            check_in_abort_on_decline,
            check_in_return_policy,
            maps,
            map_pick_count,
            map_cooldown,
            map_vote_candidates,
            map_vote_tie_break,
            match_lifetime_seconds,
            server_text,
            start_message,
            promotion_role,
            whitelist_role,
            blacklist_role,
            captain_role,
            promotion_cooldown_seconds,
            start_dm,
            show_streams,
        } = self;
        name.is_none()
            && description.is_none()
            && size.is_none()
            && team_count.is_none()
            && autostart.is_none()
            && ranked.is_none()
            && team_formation.is_none()
            && captain_mode.is_none()
            && pick_order.is_none()
            && team_names.is_none()
            && team_emojis.is_none()
            && check_in_seconds.is_none()
            && check_in_abort_on_decline.is_none()
            && check_in_return_policy.is_none()
            && maps.is_none()
            && map_pick_count.is_none()
            && map_cooldown.is_none()
            && map_vote_candidates.is_none()
            && map_vote_tie_break.is_none()
            && match_lifetime_seconds.is_none()
            && server_text.is_none()
            && start_message.is_none()
            && promotion_role.is_none()
            && whitelist_role.is_none()
            && blacklist_role.is_none()
            && captain_role.is_none()
            && promotion_cooldown_seconds.is_none()
            && start_dm.is_none()
            && show_streams.is_none()
    }

    /// Applies the patch onto `settings`.
    ///
    /// Does not validate the result; the caller runs
    /// [`QueueSettings::validate`](crate::domain::settings::QueueSettings::validate) so that every
    /// edit path checks the same
    /// rules.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Domain`] if the pick order is malformed.
    pub fn apply(&self, settings: &mut QueueSettings) -> ServiceResult<()> {
        if let Some(name) = &self.name {
            settings.name = name.clone();
        }
        if let Some(description) = &self.description {
            settings.description = Some(description.clone()).filter(|d| !d.is_empty());
        }
        if let Some(size) = self.size {
            settings.size = size;
        }
        if let Some(team_count) = self.team_count {
            settings.team_count = team_count;
        }
        if let Some(autostart) = self.autostart {
            settings.autostart = autostart;
        }
        if let Some(ranked) = self.ranked {
            settings.ranked = ranked;
        }
        if let Some(mode) = self.team_formation {
            settings.team_formation = mode;
        }
        if let Some(mode) = self.captain_mode {
            settings.captain_mode = mode;
        }
        if let Some(order) = &self.pick_order {
            settings.pick_order = PickOrder::parse(order)?;
        }
        if let Some(names) = &self.team_names {
            settings.team_names = names.clone();
        }
        if let Some(emojis) = &self.team_emojis {
            settings.team_emojis = emojis.clone();
        }
        if let Some(seconds) = self.check_in_seconds {
            settings.check_in = if seconds <= 0 {
                None
            } else {
                let mut check_in = settings.check_in.clone().unwrap_or_default();
                check_in.timeout_seconds = seconds;
                Some(check_in)
            };
        }
        if let Some(abort) = self.check_in_abort_on_decline {
            let mut check_in = settings.check_in.clone().unwrap_or_default();
            check_in.abort_on_decline = abort;
            settings.check_in = Some(check_in);
        }
        if let Some(policy) = self.check_in_return_policy {
            let mut check_in = settings.check_in.clone().unwrap_or_default();
            check_in.return_policy = policy;
            settings.check_in = Some(check_in);
        }
        if let Some(maps) = &self.maps {
            settings.maps.pool = maps.clone();
        }
        if let Some(count) = self.map_pick_count {
            settings.maps.pick_count = count;
        }
        if let Some(cooldown) = self.map_cooldown {
            settings.maps.cooldown_matches = cooldown;
        }
        if let Some(candidates) = self.map_vote_candidates {
            settings.maps.vote = if candidates == 0 {
                None
            } else {
                let mut vote = settings.maps.vote.clone().unwrap_or_default();
                vote.candidates = candidates;
                Some(vote)
            };
        }
        if let Some(tie_break) = self.map_vote_tie_break {
            let mut vote = settings.maps.vote.clone().unwrap_or_default();
            vote.tie_break = tie_break;
            settings.maps.vote = Some(vote);
        }
        if let Some(seconds) = self.match_lifetime_seconds {
            settings.match_lifetime_seconds = seconds;
        }
        if let Some(text) = &self.server_text {
            settings.server_text = Some(text.clone()).filter(|t| !t.is_empty());
        }
        if let Some(text) = &self.start_message {
            settings.start_message = Some(text.clone()).filter(|t| !t.is_empty());
        }
        if let Some(role) = self.promotion_role {
            settings.promotion_role_id = role;
        }
        if let Some(role) = self.whitelist_role {
            settings.whitelist_role_id = role;
        }
        if let Some(role) = self.blacklist_role {
            settings.blacklist_role_id = role;
        }
        if let Some(role) = self.captain_role {
            settings.captain_role_id = role;
        }
        if let Some(seconds) = self.promotion_cooldown_seconds {
            settings.promotion_cooldown_seconds = seconds;
        }
        if let Some(dm) = self.start_dm {
            settings.start_dm = dm;
        }
        if let Some(streams) = self.show_streams {
            settings.show_streams = streams;
        }
        Ok(())
    }
}

/// Fields a `/channel set` invocation may change.
/// Fields a `/channel set` invocation may change. See [`QueuePatch`] for the
/// meaning of the nested options.
#[derive(Debug, Default, Clone)]
pub struct ChannelPatch {
    /// Language for this channel. Rejected unless the catalog is installed.
    pub locale: Option<String>,
    /// Role granted administrator rights.
    pub admin_role: Option<Option<RoleId>>,
    /// Role granted moderator rights.
    pub moderator_role: Option<Option<RoleId>>,
    /// Whether to drop queued players who go offline.
    pub remove_offline: Option<bool>,
    /// Whether to drop queued players who go idle.
    pub remove_afk: Option<bool>,
    /// Whether players may use `/allow-offline`.
    pub allow_offline_opt_out: Option<bool>,
    /// Default queue expiry.
    pub default_expiry_seconds: Option<i64>,
    /// Ceiling on any requested expiry.
    pub max_expiry_seconds: Option<i64>,
    /// Ceiling on `/auto-ready`. Zero disables it.
    pub max_auto_ready_seconds: Option<i64>,
    /// Which rating algorithm to use.
    pub rating_system: Option<RatingSystemKind>,
    /// Rating given to a new player.
    pub initial_rating: Option<f64>,
    /// Deviation given to a new player.
    pub initial_deviation: Option<f64>,
    /// Floor on deviation.
    pub min_deviation: Option<f64>,
    /// Overall rating change scale.
    pub rating_scale: Option<f64>,
    /// Multiplier applied to wins.
    pub win_scale: Option<f64>,
    /// Multiplier applied to losses.
    pub loss_scale: Option<f64>,
    /// Rating change applied on a draw.
    pub draw_bonus: Option<f64>,
    /// Rating shed per inactive day.
    pub inactivity_decay_per_day: Option<f64>,
    /// Deviation regained per inactive day.
    pub deviation_decay_per_day: Option<f64>,
    /// Rank tiers. Stored sorted by rating floor.
    pub ranks: Option<Vec<RankTier>>,
    /// Whether `/nick` prefixes nicknames.
    pub rank_nickname_prefix: Option<bool>,
    /// Matches needed to appear on the leaderboard.
    pub leaderboard_min_matches: Option<i32>,
    /// Activity cutoff in days. Zero disables it.
    pub leaderboard_activity_days: Option<i32>,
    /// Share another channel's rating pool.
    pub rating_pool_channel: Option<Option<ChannelId>>,
    /// Where a live match blocks queueing.
    pub queue_scope: Option<QueueScope>,
}

impl ChannelPatch {
    /// Applies the patch onto `settings`.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Rejected`] if the requested locale is not
    /// installed. The settings are left untouched in that case.
    pub fn apply(&self, settings: &mut ChannelSettings) -> ServiceResult<()> {
        if let Some(locale) = &self.locale {
            if !locale_installed(locale) {
                return Err(ServiceError::Rejected(format!(
                    "locale {locale} is not installed; available: {}",
                    crate::localization::available_locales().join(", ")
                )));
            }
            settings.locale = locale.clone();
        }
        if let Some(role) = self.admin_role {
            settings.admin_role_id = role;
        }
        if let Some(role) = self.moderator_role {
            settings.moderator_role_id = role;
        }
        if let Some(value) = self.remove_offline {
            settings.remove_offline = value;
        }
        if let Some(value) = self.remove_afk {
            settings.remove_afk = value;
        }
        if let Some(value) = self.allow_offline_opt_out {
            settings.allow_offline_opt_out = value;
        }
        if let Some(value) = self.default_expiry_seconds {
            settings.default_expiry_seconds = value;
        }
        if let Some(value) = self.max_expiry_seconds {
            settings.max_expiry_seconds = value;
        }
        if let Some(value) = self.max_auto_ready_seconds {
            settings.max_auto_ready_seconds = value;
        }
        if let Some(system) = self.rating_system {
            settings.rating.system = system;
        }
        if let Some(value) = self.initial_rating {
            settings.rating.initial_rating = value;
        }
        if let Some(value) = self.initial_deviation {
            settings.rating.initial_deviation = value;
        }
        if let Some(value) = self.min_deviation {
            settings.rating.min_deviation = value;
        }
        if let Some(value) = self.rating_scale {
            settings.rating.scale = value;
        }
        if let Some(value) = self.win_scale {
            settings.rating.win_scale = value;
        }
        if let Some(value) = self.loss_scale {
            settings.rating.loss_scale = value;
        }
        if let Some(value) = self.draw_bonus {
            settings.rating.draw_bonus = value;
        }
        if let Some(value) = self.inactivity_decay_per_day {
            settings.rating.inactivity_decay_per_day = value;
        }
        if let Some(value) = self.deviation_decay_per_day {
            settings.rating.deviation_decay_per_day = value;
        }
        if let Some(ranks) = &self.ranks {
            let mut ranks = ranks.clone();
            ranks.sort_by_key(|tier| tier.rating_floor);
            settings.ranks = ranks;
        }
        if let Some(value) = self.rank_nickname_prefix {
            settings.rank_nickname_prefix = value;
        }
        if let Some(value) = self.leaderboard_min_matches {
            settings.leaderboard_min_matches = value;
        }
        if let Some(value) = self.leaderboard_activity_days {
            settings.leaderboard_activity_days = value;
        }
        if let Some(pool) = self.rating_pool_channel {
            settings.rating_pool_channel_id = pool;
        }
        if let Some(scope) = self.queue_scope {
            settings.queue_scope = scope;
        }
        Ok(())
    }
}

impl ConfigService {
    /// Wraps the shared application context.
    #[must_use]
    pub fn new(ctx: AppContext) -> Self {
        Self { ctx }
    }

    /// Enables PUGbot in a channel, creating its configuration at defaults if
    /// this is the first time.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Forbidden`] unless the caller is an
    /// administrator, [`ServiceError::Rejected`] if the guild is not allowed in
    /// this mode, or [`ServiceError::Database`] on failure.
    pub async fn enable_channel(
        &self,
        guild: GuildId,
        channel: ChannelId,
        actor: &Actor,
    ) -> ServiceResult<()> {
        self.ctx.ensure_guild_allowed(guild)?;
        // The channel may not exist yet, so permissions are checked against
        // whatever configuration it already had, or the defaults.
        let existing = self.ctx.store.channel_config(channel).await?;
        let settings = existing
            .as_ref()
            .map(|row| row.settings.clone())
            .unwrap_or_default();
        self.ctx
            .require_permission(actor, &settings, PermissionLevel::Administrator)?;

        self.ctx
            .store
            .enable_channel(guild, channel, &settings)
            .await?;
        self.ctx
            .audit(
                Some(guild),
                Some(channel),
                Some(actor.user),
                "channel.enabled",
                None,
                serde_json::json!({}),
            )
            .await;
        Ok(())
    }

    /// Disables PUGbot in a channel, keeping its queue, ratings, and history.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::ChannelNotEnabled`] if it was not enabled,
    /// [`ServiceError::Forbidden`] unless the caller is an administrator, or
    /// [`ServiceError::Database`] on failure.
    pub async fn disable_channel(&self, channel: ChannelId, actor: &Actor) -> ServiceResult<()> {
        let config = self.ctx.store.require_enabled_channel(channel).await?;
        self.ctx
            .require_permission(actor, &config.settings, PermissionLevel::Administrator)?;
        self.ctx.store.disable_channel(channel).await?;
        self.ctx
            .audit(
                Some(config.guild),
                Some(channel),
                Some(actor.user),
                "channel.disabled",
                None,
                serde_json::json!({}),
            )
            .await;
        Ok(())
    }

    /// Applies a channel configuration patch, returning the new settings.
    ///
    /// The before and after states are both written to the audit log.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::ChannelNotEnabled`],
    /// [`ServiceError::Forbidden`], [`ServiceError::Rejected`] for an
    /// uninstalled locale, [`ServiceError::Domain`] if the result fails
    /// validation, or [`ServiceError::Database`] on failure.
    pub async fn set_channel(
        &self,
        channel: ChannelId,
        actor: &Actor,
        patch: &ChannelPatch,
    ) -> ServiceResult<ChannelSettings> {
        let config = self.ctx.store.require_enabled_channel(channel).await?;
        self.ctx
            .require_permission(actor, &config.settings, PermissionLevel::Administrator)?;

        let before = config.settings.clone();
        let mut settings = config.settings.clone();
        patch.apply(&mut settings)?;
        settings.validate()?;
        self.ctx
            .store
            .update_channel_settings(channel, &settings)
            .await?;

        self.ctx
            .audit(
                Some(config.guild),
                Some(channel),
                Some(actor.user),
                "channel.configured",
                None,
                serde_json::json!({
                    "before": serde_json::to_value(&before).unwrap_or_default(),
                    "after": serde_json::to_value(&settings).unwrap_or_default(),
                }),
            )
            .await;
        Ok(settings)
    }

    /// Creates the channel's single queue.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::QueueExists`] if the channel already has one —
    /// a race between two administrators resolves here rather than creating
    /// two queues — plus [`ServiceError::ChannelNotEnabled`],
    /// [`ServiceError::Forbidden`], [`ServiceError::Domain`] for settings that
    /// fail validation, or [`ServiceError::Database`] on failure.
    pub async fn create_queue(
        &self,
        channel: ChannelId,
        actor: &Actor,
        patch: &QueuePatch,
    ) -> ServiceResult<QueueRow> {
        let config = self.ctx.store.require_enabled_channel(channel).await?;
        self.ctx.ensure_guild_allowed(config.guild)?;
        self.ctx
            .require_permission(actor, &config.settings, PermissionLevel::Administrator)?;

        let mut settings = QueueSettings::default();
        patch.apply(&mut settings)?;
        settings.validate()?;
        // Pick order is configuration, so an order naming a team the queue
        // does not have has to fail here rather than mid-draft.
        settings
            .pick_order
            .ensure_fits(settings.team_count as usize)?;

        self.ctx
            .store
            .create_queue(config.guild, channel, &settings)
            .await?;
        let queue = self.ctx.store.require_queue(channel).await?;

        self.ctx
            .audit(
                Some(config.guild),
                Some(channel),
                Some(actor.user),
                "queue.created",
                Some(&queue.id.to_string()),
                serde_json::to_value(&settings).unwrap_or_default(),
            )
            .await;
        Ok(queue)
    }

    /// Applies a queue configuration patch, returning the new settings.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Rejected`] for an empty patch,
    /// [`ServiceError::NoQueue`] if the channel has no queue,
    /// [`ServiceError::ChannelNotEnabled`], [`ServiceError::Forbidden`],
    /// [`ServiceError::Domain`] if the result fails validation, or
    /// [`ServiceError::Database`] on failure.
    pub async fn set_queue(
        &self,
        channel: ChannelId,
        actor: &Actor,
        patch: &QueuePatch,
    ) -> ServiceResult<QueueSettings> {
        if patch.is_empty() {
            return Err(ServiceError::Rejected(
                "no settings were provided to change".to_string(),
            ));
        }
        let config = self.ctx.store.require_enabled_channel(channel).await?;
        self.ctx
            .require_permission(actor, &config.settings, PermissionLevel::Administrator)?;
        let queue = self.ctx.store.require_queue(channel).await?;

        let before = queue.settings.clone();
        let mut settings = queue.settings.clone();
        patch.apply(&mut settings)?;
        settings.validate()?;
        settings
            .pick_order
            .ensure_fits(settings.team_count as usize)?;

        self.ctx
            .store
            .update_queue_settings(queue.id, &settings)
            .await?;
        self.ctx
            .audit(
                Some(config.guild),
                Some(channel),
                Some(actor.user),
                "queue.configured",
                Some(&queue.id.to_string()),
                serde_json::json!({
                    "before": serde_json::to_value(&before).unwrap_or_default(),
                    "after": serde_json::to_value(&settings).unwrap_or_default(),
                }),
            )
            .await;
        Ok(settings)
    }

    /// Deletes the channel's queue and its membership rows. Matches survive.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::ChannelNotEnabled`], [`ServiceError::NoQueue`],
    /// [`ServiceError::Forbidden`], or [`ServiceError::Database`].
    pub async fn delete_queue(&self, channel: ChannelId, actor: &Actor) -> ServiceResult<()> {
        let config = self.ctx.store.require_enabled_channel(channel).await?;
        self.ctx
            .require_permission(actor, &config.settings, PermissionLevel::Administrator)?;
        let queue = self.ctx.store.require_queue(channel).await?;
        self.ctx.store.delete_queue(queue.id).await?;
        self.ctx
            .audit(
                Some(config.guild),
                Some(channel),
                Some(actor.user),
                "queue.deleted",
                Some(&queue.id.to_string()),
                serde_json::json!({}),
            )
            .await;
        Ok(())
    }

    /// The channel's configuration.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::ChannelNotEnabled`] if it is not enabled.
    pub async fn channel_config(&self, channel: ChannelId) -> ServiceResult<ChannelConfigRow> {
        self.ctx.store.require_enabled_channel(channel).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::settings::{CheckInSettings, MapVoteSettings};

    #[test]
    fn an_empty_patch_is_recognised() {
        assert!(QueuePatch::default().is_empty());
        assert!(!QueuePatch {
            size: Some(10),
            ..Default::default()
        }
        .is_empty());
    }

    #[test]
    fn a_patch_only_touches_the_fields_it_sets() {
        let mut settings = QueueSettings::default();
        let original_name = settings.name.clone();
        QueuePatch {
            size: Some(12),
            ranked: Some(true),
            ..Default::default()
        }
        .apply(&mut settings)
        .unwrap();
        assert_eq!(settings.size, 12);
        assert!(settings.ranked);
        assert_eq!(settings.name, original_name);
    }

    #[test]
    fn zero_check_in_seconds_disables_check_in() {
        let mut settings = QueueSettings {
            check_in: Some(CheckInSettings::default()),
            ..Default::default()
        };
        QueuePatch {
            check_in_seconds: Some(0),
            ..Default::default()
        }
        .apply(&mut settings)
        .unwrap();
        assert!(settings.check_in.is_none());
    }

    #[test]
    fn setting_a_check_in_timeout_creates_the_block_if_absent() {
        let mut settings = QueueSettings::default();
        assert!(settings.check_in.is_none());
        QueuePatch {
            check_in_seconds: Some(45),
            ..Default::default()
        }
        .apply(&mut settings)
        .unwrap();
        assert_eq!(settings.check_in.unwrap().timeout_seconds, 45);
    }

    #[test]
    fn zero_vote_candidates_disables_the_map_vote() {
        let mut settings = QueueSettings {
            maps: crate::domain::settings::MapSettings {
                pool: vec!["a".into(), "b".into(), "c".into()],
                pick_count: 1,
                cooldown_matches: 0,
                vote: Some(MapVoteSettings::default()),
            },
            ..Default::default()
        };
        QueuePatch {
            map_vote_candidates: Some(0),
            ..Default::default()
        }
        .apply(&mut settings)
        .unwrap();
        assert!(settings.maps.vote.is_none());
    }

    #[test]
    fn a_role_can_be_cleared_as_well_as_set() {
        let mut settings = QueueSettings {
            promotion_role_id: Some(RoleId(5)),
            ..Default::default()
        };
        QueuePatch {
            promotion_role: Some(None),
            ..Default::default()
        }
        .apply(&mut settings)
        .unwrap();
        assert!(settings.promotion_role_id.is_none());

        QueuePatch {
            promotion_role: Some(Some(RoleId(9))),
            ..Default::default()
        }
        .apply(&mut settings)
        .unwrap();
        assert_eq!(settings.promotion_role_id, Some(RoleId(9)));
    }

    #[test]
    fn an_invalid_pick_order_is_rejected_by_the_patch() {
        let mut settings = QueueSettings::default();
        assert!(QueuePatch {
            pick_order: Some("A1B".into()),
            ..Default::default()
        }
        .apply(&mut settings)
        .is_err());
    }

    #[test]
    fn an_uninstalled_locale_is_rejected() {
        let mut settings = ChannelSettings::default();
        let error = ChannelPatch {
            locale: Some("kl".into()),
            ..Default::default()
        }
        .apply(&mut settings)
        .unwrap_err();
        assert!(matches!(error, ServiceError::Rejected(_)));
        assert_eq!(settings.locale, "en", "the settings must be left untouched");
    }

    #[test]
    fn an_installed_locale_is_accepted() {
        let mut settings = ChannelSettings::default();
        ChannelPatch {
            locale: Some("pt-BR".into()),
            ..Default::default()
        }
        .apply(&mut settings)
        .unwrap();
        assert_eq!(settings.locale, "pt-BR");
    }

    #[test]
    fn rank_tiers_are_stored_in_ascending_order() {
        let mut settings = ChannelSettings::default();
        ChannelPatch {
            ranks: Some(vec![
                RankTier {
                    rating_floor: 1800,
                    name: "Gold".into(),
                    emoji: None,
                    role_id: None,
                },
                RankTier {
                    rating_floor: 1000,
                    name: "Bronze".into(),
                    emoji: None,
                    role_id: None,
                },
            ]),
            ..Default::default()
        }
        .apply(&mut settings)
        .unwrap();
        assert_eq!(settings.ranks[0].name, "Bronze");
        assert_eq!(settings.ranks[1].name, "Gold");
    }

    #[test]
    fn channel_patches_reach_the_nested_rating_config() {
        let mut settings = ChannelSettings::default();
        ChannelPatch {
            rating_system: Some(RatingSystemKind::Glicko2),
            initial_rating: Some(1200.0),
            ..Default::default()
        }
        .apply(&mut settings)
        .unwrap();
        assert_eq!(settings.rating.system, RatingSystemKind::Glicko2);
        assert_eq!(settings.rating.initial_rating, 1200.0);
        settings.validate().expect("still valid");
    }
}
