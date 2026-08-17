//! Validated configuration values for channels and queues.
//!
//! These types are `serde`-serialisable because a match stores the effective
//! queue settings as a JSON snapshot: later edits to the queue must not change
//! how a historical match is interpreted.

use serde::{Deserialize, Serialize};

use crate::domain::draft::PickOrder;
use crate::domain::ids::{ChannelId, RoleId};
use crate::domain::rating::RatingConfig;
use crate::error::{DomainError, DomainResult};

/// How teams are built once a match leaves check-in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TeamFormationMode {
    /// Captains alternate picks from the unassigned pool.
    CaptainDraft,
    /// Split by smallest achievable rating difference.
    RatingMatchmaking,
    /// Shuffle into equal teams.
    RandomTeams,
    /// Announce a player list only.
    NoTeams,
}

impl TeamFormationMode {
    /// The stable string used in the persisted settings snapshot and in the
    /// slash-command choice values.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            TeamFormationMode::CaptainDraft => "captain_draft",
            TeamFormationMode::RatingMatchmaking => "rating_matchmaking",
            TeamFormationMode::RandomTeams => "random_teams",
            TeamFormationMode::NoTeams => "no_teams",
        }
    }

    /// Parses the stored form, the inverse of
    /// [`TeamFormationMode::as_str`].
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "captain_draft" => Some(TeamFormationMode::CaptainDraft),
            "rating_matchmaking" => Some(TeamFormationMode::RatingMatchmaking),
            "random_teams" => Some(TeamFormationMode::RandomTeams),
            "no_teams" => Some(TeamFormationMode::NoTeams),
            _ => None,
        }
    }
}

/// How captains are chosen when the formation mode needs them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptainMode {
    /// Prefer holders of the captain role, then highest rating.
    RoleAndRating,
    /// The pair of players with the closest ratings.
    FairPair,
    /// Random, but captain-role holders are drawn first.
    RandomWithRolePreference,
    /// Uniformly random.
    Random,
    /// Nobody is appointed; players claim slots with `/capfor`.
    Volunteer,
}

impl CaptainMode {
    /// The stable string used in the persisted settings snapshot and in the
    /// slash-command choice values.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            CaptainMode::RoleAndRating => "role_and_rating",
            CaptainMode::FairPair => "fair_pair",
            CaptainMode::RandomWithRolePreference => "random_with_role_preference",
            CaptainMode::Random => "random",
            CaptainMode::Volunteer => "volunteer",
        }
    }

    /// Parses the stored form, the inverse of [`CaptainMode::as_str`].
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "role_and_rating" => Some(CaptainMode::RoleAndRating),
            "fair_pair" => Some(CaptainMode::FairPair),
            "random_with_role_preference" => Some(CaptainMode::RandomWithRolePreference),
            "random" => Some(CaptainMode::Random),
            "volunteer" => Some(CaptainMode::Volunteer),
            _ => None,
        }
    }
}

/// What happens to players when a ready-check does not fully succeed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckInReturnPolicy {
    /// Only players who pressed Ready go back into the queue.
    ReadyOnly,
    /// Ready and silent players go back; only decliners are dropped.
    ReadyAndPending,
    /// Nobody is returned; the queue starts empty.
    None,
}

impl CheckInReturnPolicy {
    /// The stable string used in the persisted settings snapshot and in the
    /// slash-command choice values.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            CheckInReturnPolicy::ReadyOnly => "ready_only",
            CheckInReturnPolicy::ReadyAndPending => "ready_and_pending",
            CheckInReturnPolicy::None => "none",
        }
    }

    /// Parses the stored form, the inverse of
    /// [`CheckInReturnPolicy::as_str`].
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "ready_only" => Some(CheckInReturnPolicy::ReadyOnly),
            "ready_and_pending" => Some(CheckInReturnPolicy::ReadyAndPending),
            "none" => Some(CheckInReturnPolicy::None),
            _ => None,
        }
    }
}

/// Ready-check configuration for one queue.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CheckInSettings {
    /// How long players have to answer before the check-in resolves.
    pub timeout_seconds: i64,
    /// Abort as soon as somebody declines instead of waiting for the timeout.
    pub abort_on_decline: bool,
    /// Who goes back into the queue when the check-in fails.
    pub return_policy: CheckInReturnPolicy,
}

impl Default for CheckInSettings {
    fn default() -> Self {
        Self {
            timeout_seconds: 180,
            abort_on_decline: true,
            return_policy: CheckInReturnPolicy::ReadyAndPending,
        }
    }
}

/// How a tied map vote is resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TieBreak {
    /// Lowest candidate index wins — reproducible from the stored candidates.
    Deterministic,
    /// Uniform choice among the tied candidates.
    Random,
}

/// Map-vote configuration for one queue.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MapVoteSettings {
    /// Number of candidates put to the vote. Discord component limits and
    /// readability cap this at 9.
    pub candidates: u32,
    /// How a tie between candidates is broken.
    pub tie_break: TieBreak,
}

impl Default for MapVoteSettings {
    fn default() -> Self {
        Self {
            candidates: 3,
            tie_break: TieBreak::Random,
        }
    }
}

/// Map pool, cooldown, and voting configuration for one queue.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MapSettings {
    /// Every map this queue may play. Empty means the queue plays no maps.
    pub pool: Vec<String>,
    /// How many maps a match plays.
    pub pick_count: u32,
    /// Maps used in this many previous matches are avoided when the pool allows.
    pub cooldown_matches: u32,
    /// When set, candidates are put to a vote during check-in.
    pub vote: Option<MapVoteSettings>,
}

impl Default for MapSettings {
    fn default() -> Self {
        Self {
            pool: Vec::new(),
            pick_count: 1,
            cooldown_matches: 0,
            vote: None,
        }
    }
}

/// Whether a player may sit in a queue while live elsewhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueueScope {
    /// Only this channel's own queue and matches block the player.
    Channel,
    /// Any live match in the guild blocks the player from queueing.
    Guild,
}

impl QueueScope {
    /// The stable string used in the persisted settings snapshot and in the
    /// slash-command choice values.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            QueueScope::Channel => "channel",
            QueueScope::Guild => "guild",
        }
    }

    /// Parses the stored form, the inverse of [`QueueScope::as_str`].
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "channel" => Some(QueueScope::Channel),
            "guild" => Some(QueueScope::Guild),
            _ => None,
        }
    }
}

/// The effective settings of a channel's single queue.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueueSettings {
    /// Display name; the queue is still addressed by its channel.
    pub name: String,
    /// Free text shown by `/help`.
    pub description: Option<String>,
    /// Number of players required to launch.
    pub size: u32,
    /// How many teams the roster is split into. Ignored when the formation
    /// mode is [`TeamFormationMode::NoTeams`].
    pub team_count: u32,
    /// Whether a full queue launches a match on its own.
    pub autostart: bool,
    /// Whether results move ratings.
    pub ranked: bool,
    /// How teams are built once check-in passes.
    pub team_formation: TeamFormationMode,
    /// How captains are chosen, when the formation mode needs them.
    pub captain_mode: CaptainMode,
    /// The draft pick pattern, such as `ABBA`.
    pub pick_order: PickOrder,
    /// Display names per team, indexed by team number.
    pub team_names: Vec<String>,
    /// Display emoji per team, indexed by team number.
    pub team_emojis: Vec<String>,
    /// Ready-check configuration. `None` disables check-in entirely.
    pub check_in: Option<CheckInSettings>,
    /// Map pool, cooldown, and voting configuration.
    pub maps: MapSettings,
    /// A ranked match with no result after this long is expired unrated.
    pub match_lifetime_seconds: i64,
    /// Connection details shown when a match starts and by `/server`.
    pub server_text: Option<String>,
    /// Extra text shown in the match-start announcement.
    pub start_message: Option<String>,
    /// Role mentioned by `/promote` and by `/subme`.
    pub promotion_role_id: Option<RoleId>,
    /// Minimum gap between promotions, to keep `/promote` from becoming spam.
    pub promotion_cooldown_seconds: i64,
    /// Role a player must hold to join. `None` means anybody may join.
    pub whitelist_role_id: Option<RoleId>,
    /// Role that blocks joining. Checked before the whitelist, so a blacklist
    /// wins.
    pub blacklist_role_id: Option<RoleId>,
    /// Role preferred when picking captains.
    pub captain_role_id: Option<RoleId>,
    /// Whether to send match-start direct messages, subject to each player's
    /// own preference.
    pub start_dm: bool,
    /// Whether to list players who are streaming in the match announcement.
    pub show_streams: bool,
}

impl Default for QueueSettings {
    fn default() -> Self {
        Self {
            name: "pug".to_string(),
            description: None,
            size: 10,
            team_count: 2,
            autostart: true,
            ranked: false,
            team_formation: TeamFormationMode::RandomTeams,
            captain_mode: CaptainMode::Random,
            pick_order: PickOrder::default(),
            team_names: vec!["Alpha".to_string(), "Bravo".to_string()],
            team_emojis: vec!["🔵".to_string(), "🔴".to_string()],
            check_in: None,
            maps: MapSettings::default(),
            match_lifetime_seconds: 3 * 60 * 60,
            server_text: None,
            start_message: None,
            promotion_role_id: None,
            promotion_cooldown_seconds: 600,
            whitelist_role_id: None,
            blacklist_role_id: None,
            captain_role_id: None,
            start_dm: true,
            show_streams: false,
        }
    }
}

impl QueueSettings {
    /// Players per team. Zero when the queue has no teams configured.
    #[must_use]
    pub fn team_size(&self) -> u32 {
        self.size.checked_div(self.team_count).unwrap_or(0)
    }

    /// Whether this queue splits players into teams at all.
    #[must_use]
    pub fn uses_teams(&self) -> bool {
        self.team_formation != TeamFormationMode::NoTeams
    }

    /// Whether this queue needs captains appointed before play.
    #[must_use]
    pub fn needs_captains(&self) -> bool {
        self.team_formation == TeamFormationMode::CaptainDraft
    }

    /// The display label for a team: its emoji and name, or a fallback such as
    /// `Team 3` when none is configured.
    #[must_use]
    pub fn team_label(&self, index: usize) -> String {
        let name = self
            .team_names
            .get(index)
            .cloned()
            .unwrap_or_else(|| format!("Team {}", index + 1));
        match self.team_emojis.get(index) {
            Some(emoji) if !emoji.is_empty() => format!("{emoji} {name}"),
            _ => name,
        }
    }

    /// Rejects settings that would deadlock a match before anyone joins.
    ///
    /// Called on every create and every edit, so an invalid combination is
    /// refused at configuration time rather than discovered mid-match.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::InvalidConfig`] with a human-readable reason, or
    /// [`DomainError::InvalidVoteSize`] if the map vote is outside 2..=9.
    pub fn validate(&self) -> DomainResult<()> {
        if self.name.trim().is_empty() {
            return Err(DomainError::InvalidConfig("queue name is empty".into()));
        }
        if self.size < 2 {
            return Err(DomainError::InvalidConfig(
                "queue size must be at least 2".into(),
            ));
        }
        if self.size > 100 {
            return Err(DomainError::InvalidConfig(
                "queue size must be at most 100".into(),
            ));
        }
        if self.uses_teams() {
            if self.team_count < 2 {
                return Err(DomainError::InvalidConfig(
                    "a queue with teams needs at least 2 teams".into(),
                ));
            }
            if self.size % self.team_count != 0 {
                return Err(DomainError::InvalidConfig(format!(
                    "queue size {} is not divisible into {} teams",
                    self.size, self.team_count
                )));
            }
            if self.needs_captains() && self.team_size() < 1 {
                return Err(DomainError::InvalidConfig(
                    "captain draft needs at least one player per team".into(),
                ));
            }
        }
        if let Some(check_in) = &self.check_in {
            if check_in.timeout_seconds < 10 || check_in.timeout_seconds > 3600 {
                return Err(DomainError::InvalidConfig(
                    "check-in timeout must be between 10 and 3600 seconds".into(),
                ));
            }
        }
        if self.match_lifetime_seconds < 60 {
            return Err(DomainError::InvalidConfig(
                "match lifetime must be at least 60 seconds".into(),
            ));
        }
        if let Some(vote) = &self.maps.vote {
            if !(2..=9).contains(&vote.candidates) {
                return Err(DomainError::InvalidVoteSize(vote.candidates as usize));
            }
            if self.maps.pool.len() < vote.candidates as usize {
                return Err(DomainError::InvalidConfig(format!(
                    "map vote needs at least {} maps in the pool, found {}",
                    vote.candidates,
                    self.maps.pool.len()
                )));
            }
        }
        if self.maps.pick_count > 0
            && !self.maps.pool.is_empty()
            && self.maps.pick_count as usize > self.maps.pool.len()
        {
            return Err(DomainError::InvalidConfig(
                "cannot pick more maps than the pool contains".into(),
            ));
        }
        if self.promotion_cooldown_seconds < 0 {
            return Err(DomainError::InvalidConfig(
                "promotion cooldown cannot be negative".into(),
            ));
        }
        Ok(())
    }
}

/// One rank tier. Tiers are kept sorted by `rating_floor` ascending.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RankTier {
    /// Lowest rating that earns this tier.
    pub rating_floor: i32,
    /// Display name, such as `Gold`.
    pub name: String,
    /// Optional emoji shown alongside the name and in nickname prefixes.
    pub emoji: Option<String>,
    /// Discord role granted to holders of this tier, if role sync is wanted.
    pub role_id: Option<RoleId>,
}

/// Channel-wide configuration that outlives any single queue.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChannelSettings {
    /// Language used for every message in this channel. Falls back to English
    /// if the catalog is not installed.
    pub locale: String,
    /// Role granted administrator rights in this channel.
    pub admin_role_id: Option<RoleId>,
    /// Role granted moderator rights in this channel.
    pub moderator_role_id: Option<RoleId>,
    /// Whether queued players are dropped when they go offline. Requires the
    /// privileged presence intent.
    pub remove_offline: bool,
    /// Whether queued players are dropped when they go idle.
    pub remove_afk: bool,
    /// Whether players may use `/allow-offline` to opt out of presence removal.
    pub allow_offline_opt_out: bool,
    /// How long a queue slot lasts when the player expresses no preference.
    pub default_expiry_seconds: i64,
    /// Ceiling on any requested expiry, so a player cannot camp a queue
    /// indefinitely.
    pub max_expiry_seconds: i64,
    /// Ceiling on `/auto-ready`. Zero disables the feature.
    pub max_auto_ready_seconds: i64,
    /// Rating algorithm and its parameters.
    pub rating: RatingConfig,
    /// Rank tiers, kept sorted by `rating_floor` ascending.
    pub ranks: Vec<RankTier>,
    /// Whether `/nick` prefixes nicknames with the player's rating or rank.
    pub rank_nickname_prefix: bool,
    /// Matches a player needs before appearing on the leaderboard.
    pub leaderboard_min_matches: i32,
    /// Only list players active within this many days. Zero disables the
    /// cutoff.
    pub leaderboard_activity_days: i32,
    /// Share another channel's rating pool instead of keeping a private one.
    pub rating_pool_channel_id: Option<ChannelId>,
    /// Whether a live match anywhere in the guild blocks queueing here, or
    /// only a live match in this channel.
    pub queue_scope: QueueScope,
}

impl Default for ChannelSettings {
    fn default() -> Self {
        Self {
            locale: "en".to_string(),
            admin_role_id: None,
            moderator_role_id: None,
            remove_offline: false,
            remove_afk: false,
            allow_offline_opt_out: true,
            default_expiry_seconds: 4 * 60 * 60,
            max_expiry_seconds: 12 * 60 * 60,
            max_auto_ready_seconds: 60 * 60,
            rating: RatingConfig::default(),
            ranks: Vec::new(),
            rank_nickname_prefix: false,
            leaderboard_min_matches: 10,
            leaderboard_activity_days: 90,
            rating_pool_channel_id: None,
            queue_scope: QueueScope::Guild,
        }
    }
}

impl ChannelSettings {
    /// The channel whose `channel_players` rows hold this channel's ratings.
    ///
    /// Normally the channel itself; a channel may instead share another
    /// channel's pool so two queues rate into one ladder.
    #[must_use]
    pub fn rating_pool(&self, self_id: ChannelId) -> ChannelId {
        self.rating_pool_channel_id.unwrap_or(self_id)
    }

    /// The tier a rating earns, or `None` if it is below every floor.
    #[must_use]
    pub fn rank_for(&self, rating: f64) -> Option<&RankTier> {
        self.ranks
            .iter()
            .filter(|tier| rating >= f64::from(tier.rating_floor))
            .max_by_key(|tier| tier.rating_floor)
    }

    /// Rejects a channel configuration that cannot work.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::InvalidConfig`] for an empty locale, an expiry
    /// ceiling below the default, negative values where none are meaningful,
    /// or duplicated rank floors. Also propagates
    /// [`RatingConfig::validate`].
    pub fn validate(&self) -> DomainResult<()> {
        if self.locale.trim().is_empty() {
            return Err(DomainError::InvalidConfig("locale is empty".into()));
        }
        if self.default_expiry_seconds < 60 {
            return Err(DomainError::InvalidConfig(
                "default expiry must be at least 60 seconds".into(),
            ));
        }
        if self.max_expiry_seconds < self.default_expiry_seconds {
            return Err(DomainError::InvalidConfig(
                "maximum expiry must not be below the default expiry".into(),
            ));
        }
        if self.max_auto_ready_seconds < 0 {
            return Err(DomainError::InvalidConfig(
                "auto-ready maximum cannot be negative".into(),
            ));
        }
        if self.leaderboard_min_matches < 0 {
            return Err(DomainError::InvalidConfig(
                "leaderboard minimum matches cannot be negative".into(),
            ));
        }
        let mut floors: Vec<i32> = self.ranks.iter().map(|tier| tier.rating_floor).collect();
        floors.sort_unstable();
        floors.dedup();
        if floors.len() != self.ranks.len() {
            return Err(DomainError::InvalidConfig(
                "rank tiers must have distinct rating floors".into(),
            ));
        }
        self.rating.validate()
    }

    /// Clamps a requested expiry to the channel maximum, and to a floor of one
    /// minute so a typo cannot create a slot that expires instantly.
    #[must_use]
    pub fn clamp_expiry(&self, requested_seconds: i64) -> i64 {
        requested_seconds.clamp(60, self.max_expiry_seconds)
    }

    /// Clamps a requested auto-ready duration to the channel maximum. A result
    /// of zero means the feature is switched off here.
    #[must_use]
    pub fn clamp_auto_ready(&self, requested_seconds: i64) -> i64 {
        requested_seconds.clamp(0, self.max_auto_ready_seconds)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_queue_settings_are_valid() {
        QueueSettings::default().validate().expect("sane defaults");
        ChannelSettings::default()
            .validate()
            .expect("sane defaults");
    }

    #[test]
    fn size_must_divide_evenly_into_teams() {
        let mut settings = QueueSettings {
            size: 9,
            ..Default::default()
        };
        assert!(settings.validate().is_err());
        settings.team_formation = TeamFormationMode::NoTeams;
        settings.validate().expect("no_teams ignores divisibility");
    }

    #[test]
    fn map_vote_requires_enough_maps_in_the_pool() {
        let settings = QueueSettings {
            maps: MapSettings {
                pool: vec!["de_dust2".into(), "de_inferno".into()],
                pick_count: 1,
                cooldown_matches: 0,
                vote: Some(MapVoteSettings {
                    candidates: 3,
                    tie_break: TieBreak::Deterministic,
                }),
            },
            ..Default::default()
        };
        assert!(settings.validate().is_err());
    }

    #[test]
    fn vote_candidates_are_bounded_to_the_component_limit() {
        let settings = QueueSettings {
            maps: MapSettings {
                pool: (0..20).map(|i| format!("map{i}")).collect(),
                pick_count: 1,
                cooldown_matches: 0,
                vote: Some(MapVoteSettings {
                    candidates: 10,
                    tie_break: TieBreak::Random,
                }),
            },
            ..Default::default()
        };
        assert_eq!(
            settings.validate().unwrap_err(),
            DomainError::InvalidVoteSize(10)
        );
    }

    #[test]
    fn rank_lookup_picks_the_highest_matching_floor() {
        let settings = ChannelSettings {
            ranks: vec![
                RankTier {
                    rating_floor: 1000,
                    name: "Bronze".into(),
                    emoji: None,
                    role_id: None,
                },
                RankTier {
                    rating_floor: 1400,
                    name: "Silver".into(),
                    emoji: None,
                    role_id: None,
                },
                RankTier {
                    rating_floor: 1800,
                    name: "Gold".into(),
                    emoji: None,
                    role_id: None,
                },
            ],
            ..Default::default()
        };
        assert_eq!(settings.rank_for(999.0), None);
        assert_eq!(settings.rank_for(1000.0).unwrap().name, "Bronze");
        assert_eq!(settings.rank_for(1750.0).unwrap().name, "Silver");
        assert_eq!(settings.rank_for(9999.0).unwrap().name, "Gold");
    }

    #[test]
    fn duplicate_rank_floors_are_rejected() {
        let settings = ChannelSettings {
            ranks: vec![
                RankTier {
                    rating_floor: 1000,
                    name: "A".into(),
                    emoji: None,
                    role_id: None,
                },
                RankTier {
                    rating_floor: 1000,
                    name: "B".into(),
                    emoji: None,
                    role_id: None,
                },
            ],
            ..Default::default()
        };
        assert!(settings.validate().is_err());
    }

    #[test]
    fn expiry_is_clamped_to_the_channel_maximum() {
        let settings = ChannelSettings::default();
        assert_eq!(
            settings.clamp_expiry(999_999),
            settings.max_expiry_seconds,
            "a player cannot outlive the channel cap"
        );
        assert_eq!(settings.clamp_expiry(1), 60);
    }
}
