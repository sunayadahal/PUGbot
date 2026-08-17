//! Interaction dispatch.
//!
//! Handlers translate a Discord interaction into a service call and a reply.
//! They contain no rules of their own: permissions, state checks and validation
//! all live in the services and the domain.

use serenity::all::{
    CommandDataOption, CommandDataOptionValue, CommandInteraction, ComponentInteraction,
    CreateActionRow, CreateEmbed, Permissions,
};

use super::embeds::{self, custom_id};
use crate::domain::ids::{ChannelId, GuildId, RoleId, UserId};
use crate::domain::permissions::{Actor, PermissionLevel};
use crate::domain::rating::RatingSystemKind;
use crate::domain::report::ReportOutcome;
use crate::domain::settings::{
    CaptainMode, CheckInReturnPolicy, QueueScope, TeamFormationMode, TieBreak,
};
use crate::error::{ServiceError, ServiceResult};
use crate::localization::Locale;
use crate::services::config_svc::{ChannelPatch, ConfigService, QueuePatch};
use crate::services::match_svc::{historical_settings, MatchService, ReportStatus};
use crate::services::moderation_svc::ModerationService;
use crate::services::queue_svc::QueueService;
use crate::services::rating_svc::RatingService;
use crate::services::{humanize_seconds, AppContext};

const LEADERBOARD_PAGE_SIZE: i64 = 15;

/// What the adapter should send back.
#[derive(Debug, Default)]
pub struct Reply {
    /// Message text, if any.
    pub content: Option<String>,
    /// Embeds to attach.
    pub embeds: Vec<CreateEmbed>,
    /// Interactive components to attach.
    pub components: Vec<CreateActionRow>,
    /// Whether only the caller sees this. Errors and personal settings do.
    pub ephemeral: bool,
}

impl Reply {
    /// Visible to everyone in the channel.
    #[must_use]
    pub fn public(content: impl Into<String>) -> Self {
        Self {
            content: Some(content.into()),
            ephemeral: false,
            ..Default::default()
        }
    }

    /// Visible only to the caller. Errors and personal settings use this.
    #[must_use]
    pub fn private(content: impl Into<String>) -> Self {
        Self {
            content: Some(content.into()),
            ephemeral: true,
            ..Default::default()
        }
    }

    /// A public reply carrying a single embed.
    #[must_use]
    pub fn embed(embed: CreateEmbed) -> Self {
        Self {
            embeds: vec![embed],
            ..Default::default()
        }
    }

    /// Attaches interactive components.
    #[must_use]
    pub fn with_components(mut self, components: Vec<CreateActionRow>) -> Self {
        self.components = components;
        self
    }

    /// An ephemeral reply carrying a single embed.
    #[must_use]
    pub fn private_embed(embed: CreateEmbed) -> Self {
        Self {
            embeds: vec![embed],
            ephemeral: true,
            ..Default::default()
        }
    }
}

/// Convenience accessor over a command's options, including subcommands.
struct Opts<'a>(&'a [CommandDataOption]);

impl<'a> Opts<'a> {
    fn find(&self, name: &str) -> Option<&'a CommandDataOptionValue> {
        self.0
            .iter()
            .find(|option| option.name == name)
            .map(|option| &option.value)
    }

    fn string(&self, name: &str) -> Option<&'a str> {
        match self.find(name)? {
            CommandDataOptionValue::String(value) => Some(value.as_str()),
            _ => None,
        }
    }

    fn integer(&self, name: &str) -> Option<i64> {
        match self.find(name)? {
            CommandDataOptionValue::Integer(value) => Some(*value),
            _ => None,
        }
    }

    fn number(&self, name: &str) -> Option<f64> {
        match self.find(name)? {
            CommandDataOptionValue::Number(value) => Some(*value),
            CommandDataOptionValue::Integer(value) => Some(*value as f64),
            _ => None,
        }
    }

    fn boolean(&self, name: &str) -> Option<bool> {
        match self.find(name)? {
            CommandDataOptionValue::Boolean(value) => Some(*value),
            _ => None,
        }
    }

    fn user(&self, name: &str) -> Option<UserId> {
        match self.find(name)? {
            CommandDataOptionValue::User(value) => Some(UserId::from(value.get())),
            _ => None,
        }
    }

    fn role(&self, name: &str) -> Option<RoleId> {
        match self.find(name)? {
            CommandDataOptionValue::Role(value) => Some(RoleId::from(value.get())),
            _ => None,
        }
    }

    fn channel(&self, name: &str) -> Option<ChannelId> {
        match self.find(name)? {
            CommandDataOptionValue::Channel(value) => Some(ChannelId::from(value.get())),
            _ => None,
        }
    }

    /// The invoked subcommand, if this command has one.
    fn subcommand(&self) -> Option<(&'a str, Opts<'a>)> {
        self.0.iter().find_map(|option| match &option.value {
            CommandDataOptionValue::SubCommand(nested)
            | CommandDataOptionValue::SubCommandGroup(nested) => {
                Some((option.name.as_str(), Opts(nested)))
            }
            _ => None,
        })
    }
}

/// Splits a comma-separated option into trimmed, non-empty entries.
fn comma_list(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect()
}

/// Parses a whitespace-separated list of mentions or raw IDs.
fn parse_user_list(raw: &str) -> ServiceResult<Vec<UserId>> {
    let mut users = Vec::new();
    for token in raw.split_whitespace() {
        let digits: String = token.chars().filter(char::is_ascii_digit).collect();
        let id: i64 = digits.parse().map_err(|_| {
            ServiceError::Rejected(format!("{token:?} is not a user mention or ID"))
        })?;
        users.push(UserId(id));
    }
    if users.is_empty() {
        return Err(ServiceError::Rejected("no players were listed".to_string()));
    }
    let mut unique = users.clone();
    unique.sort_unstable();
    unique.dedup();
    if unique.len() != users.len() {
        return Err(ServiceError::Rejected(
            "the same player is listed twice".to_string(),
        ));
    }
    Ok(users)
}

/// Builds the domain actor from the interaction's member data.
///
/// Native Discord permissions are read from the member payload Discord attaches
/// to the interaction, so no extra API call is needed.
#[must_use]
pub fn actor_from(app: &AppContext, interaction: &CommandInteraction) -> Actor {
    let (roles, permissions) = interaction.member.as_ref().map_or_else(
        || (Vec::new(), Permissions::empty()),
        |member| {
            (
                member
                    .roles
                    .iter()
                    .map(|role| RoleId::from(role.get()))
                    .collect(),
                member.permissions.unwrap_or_else(Permissions::empty),
            )
        },
    );
    app.actor(Actor {
        user: UserId::from(interaction.user.id.get()),
        roles,
        is_guild_admin: permissions.administrator() || permissions.manage_guild(),
        is_guild_moderator: permissions.manage_messages(),
        is_bot_owner: false,
    })
}

/// Handles one slash command, returning what to reply.
///
/// # Errors
///
/// Returns whatever the underlying service returned. The caller renders it
/// through [`error_reply`] rather than propagating.
pub async fn handle_command(
    app: &AppContext,
    interaction: &CommandInteraction,
) -> ServiceResult<Reply> {
    let Some(guild_id) = interaction.guild_id else {
        return Err(ServiceError::Rejected(
            "PUGbot commands only work inside a server".to_string(),
        ));
    };
    let guild = GuildId::from(guild_id.get());
    app.ensure_guild_allowed(guild)?;

    let channel = ChannelId::from(interaction.channel_id.get());
    let actor = actor_from(app, interaction);
    let opts = Opts(&interaction.data.options);

    match interaction.data.name.as_str() {
        "channel" => channel_command(app, guild, channel, &actor, &opts).await,
        "queue" => queue_command(app, channel, &actor, &opts).await,
        "match" => match_command(app, channel, &actor, &opts).await,
        "rating" => rating_command(app, channel, &actor, &opts).await,
        "stats" => stats_command(app, channel, &actor, &opts).await,
        "noadds" => noadds_command(app, guild, channel, &actor, &opts).await,
        "phrases" => phrases_command(app, channel, &actor, &opts).await,
        name => player_command(app, name, channel, &actor, &opts).await,
    }
}

// ------------------------------------------------------------ player commands

async fn player_command(
    app: &AppContext,
    name: &str,
    channel: ChannelId,
    actor: &Actor,
    opts: &Opts<'_>,
) -> ServiceResult<Reply> {
    let queues = QueueService::new(app.clone());
    let matches = MatchService::new(app.clone());
    let moderation = ModerationService::new(app.clone());
    let ratings = RatingService::new(app.clone());

    match name {
        "add" => {
            let context = queues.context(channel).await?;
            let locale = app.locale(&context.channel.settings);
            let result = queues
                .add(&context, actor.user, &actor.roles, opts.integer("expire"))
                .await?;
            let mut text = locale.format(
                "queue.joined",
                &[
                    ("user", &format!("<@{}>", actor.user)),
                    ("current", &result.snapshot.len().to_string()),
                    ("size", &context.queue.settings.size.to_string()),
                ],
            );
            if let Some(phrase) = &result.phrase {
                text.push_str(&format!("\n{phrase}"));
            }
            if let Some(id) = result.started {
                text.push_str(&format!(
                    "\n{}",
                    locale.format("match.started", &[("id", &id.to_string())],)
                ));
            }
            Ok(Reply::public(text))
        }
        "remove" => {
            let context = queues.context(channel).await?;
            let locale = app.locale(&context.channel.settings);
            let snapshot = queues.remove(&context, actor.user).await?;
            Ok(Reply::public(locale.format(
                "queue.left",
                &[
                    ("user", &format!("<@{}>", actor.user)),
                    ("current", &snapshot.len().to_string()),
                    ("size", &context.queue.settings.size.to_string()),
                ],
            )))
        }
        "who" => {
            let context = queues.context(channel).await?;
            let locale = app.locale(&context.channel.settings);
            let snapshot = queues.snapshot(&context).await?;
            Ok(Reply::embed(embeds::queue_embed(
                &snapshot,
                &context.queue.settings,
                locale,
            )))
        }
        "promote" => {
            let context = queues.context(channel).await?;
            let locale = app.locale(&context.channel.settings);
            let result = queues.promote(&context).await?;
            let mut text = locale.format(
                "queue.promoted",
                &[
                    ("queue", &context.queue.settings.name),
                    ("needed", &result.needed.to_string()),
                ],
            );
            if let Some(role) = result.role {
                text = format!("<@&{role}> {text}");
            }
            Ok(Reply::public(text))
        }
        "subscribe" => {
            let config = app.store.require_enabled_channel(channel).await?;
            let locale = app.locale(&config.settings);
            moderation.subscribe(&config, actor.user).await?;
            Ok(Reply::private(locale.get("prefs.subscribed")))
        }
        "unsubscribe" => {
            let config = app.store.require_enabled_channel(channel).await?;
            let locale = app.locale(&config.settings);
            moderation.unsubscribe(&config, actor.user).await?;
            Ok(Reply::private(locale.get("prefs.unsubscribed")))
        }
        "server" => {
            let context = queues.context(channel).await?;
            Ok(Reply::private(
                context
                    .queue
                    .settings
                    .server_text
                    .clone()
                    .unwrap_or_else(|| "No server details are configured.".to_string()),
            ))
        }
        "maps" => {
            let context = queues.context(channel).await?;
            let pool = &context.queue.settings.maps.pool;
            Ok(Reply::private(if pool.is_empty() {
                "This queue has no map pool.".to_string()
            } else {
                pool.join(", ")
            }))
        }
        "map" => {
            let context = queues.context(channel).await?;
            let pool = context.queue.settings.maps.pool.clone();
            let recent = app.store.recent_maps(channel, 20).await?;
            let cooldown = context.queue.settings.maps.cooldown_matches as usize;
            let picked = {
                let mut rng = rand::thread_rng();
                crate::domain::maps::select_maps(&pool, 1, &recent, cooldown, &mut rng)?
            };
            Ok(Reply::public(format!("**{}**", picked.join(", "))))
        }
        "ready" | "notready" => {
            let config = app.store.require_enabled_channel(channel).await?;
            let locale = app.locale(&config.settings);
            let live = require_live_match(app, actor.user, channel, &config).await?;
            matches
                .set_ready(live.info.id, actor.user, name == "ready")
                .await?;
            Ok(Reply::private(locale.get(if name == "ready" {
                "checkin.you_are_ready"
            } else {
                "checkin.you_declined"
            })))
        }
        "teams" => {
            let config = app.store.require_enabled_channel(channel).await?;
            let locale = app.locale(&config.settings);
            let live = require_live_match(app, actor.user, channel, &config).await?;
            Ok(Reply::embed(embeds::match_embed(&live, locale, None)))
        }
        "matches" => {
            let config = app.store.require_enabled_channel(channel).await?;
            let locale = app.locale(&config.settings);
            let live = app.store.live_matches(channel).await?;
            if live.is_empty() {
                return Ok(Reply::private(locale.get("match.none_active")));
            }
            Ok(Reply {
                embeds: live
                    .iter()
                    .take(5)
                    .map(|loaded| embeds::match_embed(loaded, locale, None))
                    .collect(),
                ..Default::default()
            })
        }
        "capfor" => {
            let config = app.store.require_enabled_channel(channel).await?;
            let locale = app.locale(&config.settings);
            let live = require_live_match(app, actor.user, channel, &config).await?;
            // Teams are 1-based for players and 0-based internally.
            let team = opts.integer("team").unwrap_or(1).max(1) as usize - 1;
            matches
                .claim_captain(live.info.id, actor.user, team)
                .await?;
            Ok(Reply::public(locale.format(
                "draft.captain_claimed",
                &[
                    ("user", &format!("<@{}>", actor.user)),
                    ("team", &live.info.settings.team_label(team)),
                ],
            )))
        }
        "capme" => {
            let config = app.store.require_enabled_channel(channel).await?;
            let locale = app.locale(&config.settings);
            let live = require_live_match(app, actor.user, channel, &config).await?;
            matches.vacate_captain(live.info.id, actor.user).await?;
            Ok(Reply::public(locale.format(
                "draft.captain_vacated",
                &[("user", &format!("<@{}>", actor.user))],
            )))
        }
        "pick" => {
            let config = app.store.require_enabled_channel(channel).await?;
            let locale = app.locale(&config.settings);
            let live = require_live_match(app, actor.user, channel, &config).await?;
            let player = opts
                .user("player")
                .ok_or_else(|| ServiceError::Rejected("no player was given".to_string()))?;
            matches.pick(live.info.id, actor.user, player).await?;
            Ok(Reply::public(locale.format(
                "draft.picked",
                &[
                    ("captain", &format!("<@{}>", actor.user)),
                    ("player", &format!("<@{player}>")),
                ],
            )))
        }
        "subme" => {
            let config = app.store.require_enabled_channel(channel).await?;
            let locale = app.locale(&config.settings);
            let live = require_live_match(app, actor.user, channel, &config).await?;
            let mut text = locale.format(
                "match.sub_requested",
                &[
                    ("user", &format!("<@{}>", actor.user)),
                    ("id", &live.info.id.to_string()),
                ],
            );
            if let Some(role) = live.info.settings.promotion_role_id {
                text = format!("<@&{role}> {text}");
            }
            Ok(Reply::public(text))
        }
        "subfor" => {
            let config = app.store.require_enabled_channel(channel).await?;
            let locale = app.locale(&config.settings);
            let out = opts
                .user("player")
                .ok_or_else(|| ServiceError::Rejected("no player was given".to_string()))?;
            let live = matches
                .live_match_for(out, channel, &config.settings, config.guild)
                .await?
                .ok_or(ServiceError::NoMatch)?;
            matches.substitute(live.info.id, out, actor.user).await?;
            Ok(Reply::public(locale.format(
                "match.sub_done",
                &[
                    ("into", &format!("<@{}>", actor.user)),
                    ("out", &format!("<@{out}>")),
                    ("id", &live.info.id.to_string()),
                ],
            )))
        }
        "report" => {
            let config = app.store.require_enabled_channel(channel).await?;
            let locale = app.locale(&config.settings);
            let live = require_live_match(app, actor.user, channel, &config).await?;
            let choice = opts.string("result").unwrap_or("win");
            let own_team = live.team_of(actor.user).unwrap_or(0);
            let outcome = match choice {
                "win" => ReportOutcome::Win(own_team),
                "loss" => {
                    // With two teams "the other team won" is unambiguous; with
                    // more, the player has to say which.
                    if live.info.settings.team_count != 2 {
                        return Err(ServiceError::Rejected(
                            "with more than two teams a moderator must report the winner"
                                .to_string(),
                        ));
                    }
                    ReportOutcome::Win(1 - own_team)
                }
                "draw" => ReportOutcome::Draw,
                _ => ReportOutcome::Cancel,
            };
            let status = matches.report(live.info.id, actor.user, outcome).await?;
            Ok(match status {
                ReportStatus::Pending => Reply::private(locale.get("report.recorded")),
                ReportStatus::Disputed => Reply::public(
                    locale.format("report.disputed", &[("id", &live.info.id.to_string())]),
                ),
                ReportStatus::Final(outcome) => Reply::public(locale.format(
                    "report.final",
                    &[
                        ("id", &live.info.id.to_string()),
                        (
                            "result",
                            &describe_outcome(outcome, &live.info.settings, locale),
                        ),
                    ],
                )),
            })
        }
        "rank" => {
            let config = app.store.require_enabled_channel(channel).await?;
            let locale = app.locale(&config.settings);
            let target = opts.user("player").unwrap_or(actor.user);
            let view = ratings.rank(&config, target).await?;
            Ok(Reply::embed(embeds::rank_embed(&view, locale)))
        }
        "leaderboard" => {
            let config = app.store.require_enabled_channel(channel).await?;
            let locale = app.locale(&config.settings);
            let page = opts.integer("page").unwrap_or(1).max(1);
            let (rows, total) = ratings
                .leaderboard(&config, page, LEADERBOARD_PAGE_SIZE)
                .await?;
            Ok(Reply::embed(embeds::leaderboard_embed(
                &rows,
                page,
                LEADERBOARD_PAGE_SIZE,
                total,
                locale,
            )))
        }
        "top" => {
            let config = app.store.require_enabled_channel(channel).await?;
            let locale = app.locale(&config.settings);
            let rows = ratings.most_active(&config, 15).await?;
            if rows.is_empty() {
                return Ok(Reply::private(locale.get("rating.leaderboard_empty")));
            }
            let body = rows
                .iter()
                .map(|row| {
                    format!(
                        "<@{}> — {} matches ({:.0})",
                        row.user,
                        row.matches_played(),
                        row.rating
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            Ok(Reply::embed(
                CreateEmbed::new()
                    .title(locale.get("rating.top_title"))
                    .description(body),
            ))
        }
        "lastgame" => {
            let config = app.store.require_enabled_channel(channel).await?;
            let locale = app.locale(&config.settings);
            let recent = app
                .store
                .recent_matches(channel, opts.user("player"), 1)
                .await?;
            match recent.first() {
                Some(loaded) => Ok(Reply::embed(embeds::match_embed(loaded, locale, None))),
                None => Ok(Reply::private(locale.get("error.no_match"))),
            }
        }
        "nick" => {
            let config = app.store.require_enabled_channel(channel).await?;
            if !config.settings.rank_nickname_prefix {
                return Err(ServiceError::Rejected(
                    "this channel does not use rating nicknames".to_string(),
                ));
            }
            let view = ratings.rank(&config, actor.user).await?;
            let prefix = crate::services::rating_svc::nickname_prefix(
                view.rank.as_ref(),
                view.stats.rating,
                config.settings.rating.system,
            );
            // Applying the nickname needs the gateway; the adapter does it and
            // this reply tells the player what was applied.
            Ok(Reply::private(format!("Nickname prefix: `{prefix}`")))
        }
        "help" => {
            let config = app.store.require_enabled_channel(channel).await?;
            let locale = app.locale(&config.settings);
            let queue = app.store.queue_for_channel(channel).await?;
            let mut body = locale.get("help.body").to_string();
            if let Some(description) = queue.and_then(|q| q.settings.description) {
                body = format!("{description}\n\n{body}");
            }
            Ok(Reply::private_embed(
                CreateEmbed::new()
                    .title(locale.get("help.title"))
                    .description(body),
            ))
        }
        "commands" => {
            let config = app.store.require_enabled_channel(channel).await?;
            let locale = app.locale(&config.settings);
            Ok(Reply::private(locale.get("help.commands")))
        }
        "switch-dms" => {
            let config = app.store.require_enabled_channel(channel).await?;
            let locale = app.locale(&config.settings);
            let enabled = moderation.toggle_dms(actor.user).await?;
            Ok(Reply::private(locale.get(if enabled {
                "prefs.dms_on"
            } else {
                "prefs.dms_off"
            })))
        }
        "expire" => {
            let config = app.store.require_enabled_channel(channel).await?;
            let locale = app.locale(&config.settings);
            let seconds = opts.integer("seconds").unwrap_or(0);
            let expires_at = moderation
                .set_session_expiry(&config, actor.user, seconds)
                .await?;
            Ok(Reply::private(match expires_at {
                Some(at) => locale.format(
                    "queue.expiry_set",
                    &[(
                        "seconds",
                        &(at - app.now()).num_seconds().max(0).to_string(),
                    )],
                ),
                None => "Your queue slot will not expire.".to_string(),
            }))
        }
        "expire-default" => {
            let config = app.store.require_enabled_channel(channel).await?;
            let seconds = opts.integer("seconds").unwrap_or(0);
            let stored = moderation
                .set_default_expiry(&config, actor.user, Some(seconds))
                .await?;
            Ok(Reply::private(match stored {
                Some(0) | None => "Your queue slots will not expire by default.".to_string(),
                Some(value) => format!("Default queue expiry set to {}.", humanize_seconds(value)),
            }))
        }
        "auto-ready" => {
            let config = app.store.require_enabled_channel(channel).await?;
            let locale = app.locale(&config.settings);
            let seconds = opts.integer("seconds").unwrap_or(0);
            let applied = moderation
                .arm_auto_ready(&config, actor.user, seconds)
                .await?;
            Ok(Reply::private(locale.format(
                "prefs.auto_ready",
                &[("seconds", &applied.to_string())],
            )))
        }
        "allow-offline" => {
            let config = app.store.require_enabled_channel(channel).await?;
            let locale = app.locale(&config.settings);
            let seconds = opts.integer("seconds").unwrap_or(0);
            let applied = moderation
                .allow_offline(&config, actor.user, seconds)
                .await?;
            Ok(Reply::private(locale.format(
                "prefs.allow_offline",
                &[("seconds", &applied.to_string())],
            )))
        }
        other => Err(ServiceError::Rejected(format!("unknown command /{other}"))),
    }
}

/// The caller's live match in this channel, or a clear rejection.
async fn require_live_match(
    app: &AppContext,
    user: UserId,
    channel: ChannelId,
    config: &crate::repositories::ChannelConfigRow,
) -> ServiceResult<crate::repositories::matches::LoadedMatch> {
    MatchService::new(app.clone())
        .live_match_for(user, channel, &config.settings, config.guild)
        .await?
        .ok_or(ServiceError::NoMatch)
}

fn describe_outcome(
    outcome: ReportOutcome,
    settings: &crate::domain::settings::QueueSettings,
    locale: Locale,
) -> String {
    match outcome {
        ReportOutcome::Win(team) => {
            locale.format("report.result_win", &[("team", &settings.team_label(team))])
        }
        ReportOutcome::Draw => locale.get("report.result_draw").to_string(),
        ReportOutcome::Cancel => locale.get("report.result_cancel").to_string(),
    }
}

// ------------------------------------------------------------- /channel

async fn channel_command(
    app: &AppContext,
    guild: GuildId,
    channel: ChannelId,
    actor: &Actor,
    opts: &Opts<'_>,
) -> ServiceResult<Reply> {
    let config_svc = ConfigService::new(app.clone());
    let (name, sub) = opts
        .subcommand()
        .ok_or_else(|| ServiceError::Rejected("no subcommand given".to_string()))?;

    match name {
        "enable" => {
            config_svc.enable_channel(guild, channel, actor).await?;
            let config = app.store.require_enabled_channel(channel).await?;
            Ok(Reply::public(
                app.locale(&config.settings).get("config.channel_enabled"),
            ))
        }
        "disable" => {
            let config = app.store.require_enabled_channel(channel).await?;
            let locale = app.locale(&config.settings);
            config_svc.disable_channel(channel, actor).await?;
            Ok(Reply::public(locale.get("config.channel_disabled")))
        }
        "show" => {
            let config = config_svc.channel_config(channel).await?;
            app.require_permission(actor, &config.settings, PermissionLevel::Moderator)?;
            let rendered = serde_json::to_string_pretty(&config.settings)
                .map_err(|e| ServiceError::Other(e.into()))?;
            Ok(Reply::private(format!(
                "```json\n{}\n```",
                fit_code(&rendered)
            )))
        }
        "set" => {
            let patch = channel_patch(&sub);
            let settings = config_svc.set_channel(channel, actor, &patch).await?;
            Ok(Reply::private(app.locale(&settings).get("config.updated")))
        }
        other => Err(ServiceError::Rejected(format!(
            "unknown subcommand {other}"
        ))),
    }
}

fn channel_patch(opts: &Opts<'_>) -> ChannelPatch {
    ChannelPatch {
        locale: opts.string("locale").map(str::to_string),
        admin_role: opts.role("admin-role").map(Some),
        moderator_role: opts.role("moderator-role").map(Some),
        remove_offline: opts.boolean("remove-offline"),
        remove_afk: opts.boolean("remove-afk"),
        allow_offline_opt_out: opts.boolean("allow-offline-opt-out"),
        default_expiry_seconds: opts.integer("default-expiry"),
        max_expiry_seconds: opts.integer("max-expiry"),
        max_auto_ready_seconds: opts.integer("max-auto-ready"),
        rating_system: opts
            .string("rating-system")
            .and_then(RatingSystemKind::parse),
        initial_rating: opts.number("initial-rating"),
        initial_deviation: opts.number("initial-deviation"),
        min_deviation: opts.number("min-deviation"),
        rating_scale: opts.number("rating-scale"),
        win_scale: opts.number("win-scale"),
        loss_scale: opts.number("loss-scale"),
        draw_bonus: opts.number("draw-bonus"),
        inactivity_decay_per_day: opts.number("decay-per-day"),
        deviation_decay_per_day: opts.number("deviation-decay-per-day"),
        ranks: None,
        rank_nickname_prefix: opts.boolean("rank-nicknames"),
        leaderboard_min_matches: opts
            .integer("leaderboard-min-matches")
            .map(|value| value as i32),
        leaderboard_activity_days: opts
            .integer("leaderboard-activity-days")
            .map(|value| value as i32),
        rating_pool_channel: opts.channel("rating-pool").map(Some),
        queue_scope: opts.string("queue-scope").and_then(QueueScope::parse),
    }
}

/// Keeps a configuration dump inside Discord's 2000-character message limit.
fn fit_code(text: &str) -> String {
    const LIMIT: usize = 1800;
    if text.len() <= LIMIT {
        return text.to_string();
    }
    let mut end = LIMIT;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n…", &text[..end])
}

// --------------------------------------------------------------- /queue

async fn queue_command(
    app: &AppContext,
    channel: ChannelId,
    actor: &Actor,
    opts: &Opts<'_>,
) -> ServiceResult<Reply> {
    let config_svc = ConfigService::new(app.clone());
    let queues = QueueService::new(app.clone());
    let (name, sub) = opts
        .subcommand()
        .ok_or_else(|| ServiceError::Rejected("no subcommand given".to_string()))?;

    match name {
        "create" => {
            let patch = queue_patch(&sub);
            let queue = config_svc.create_queue(channel, actor, &patch).await?;
            let config = app.store.require_enabled_channel(channel).await?;
            Ok(Reply::public(app.locale(&config.settings).format(
                "config.queue_created",
                &[("queue", &queue.settings.name)],
            )))
        }
        "set-basics" | "set-teams" | "set-maps" | "set-roles" => {
            let patch = queue_patch(&sub);
            config_svc.set_queue(channel, actor, &patch).await?;
            let config = app.store.require_enabled_channel(channel).await?;
            Ok(Reply::private(
                app.locale(&config.settings).get("config.updated"),
            ))
        }
        "show" => {
            let context = queues.context(channel).await?;
            app.require_permission(actor, &context.channel.settings, PermissionLevel::Moderator)?;
            let rendered = serde_json::to_string_pretty(&context.queue.settings)
                .map_err(|e| ServiceError::Other(e.into()))?;
            Ok(Reply::private(format!(
                "```json\n{}\n```",
                fit_code(&rendered)
            )))
        }
        "delete" => {
            let config = app.store.require_enabled_channel(channel).await?;
            let locale = app.locale(&config.settings);
            config_svc.delete_queue(channel, actor).await?;
            Ok(Reply::public(locale.get("config.queue_deleted")))
        }
        "add-player" => {
            let context = queues.context(channel).await?;
            let locale = app.locale(&context.channel.settings);
            let player = sub
                .user("player")
                .ok_or_else(|| ServiceError::Rejected("no player was given".to_string()))?;
            let result = queues.force_add(&context, actor, player).await?;
            Ok(Reply::public(locale.format(
                "queue.joined",
                &[
                    ("user", &format!("<@{player}>")),
                    ("current", &result.snapshot.len().to_string()),
                    ("size", &context.queue.settings.size.to_string()),
                ],
            )))
        }
        "remove-player" => {
            let context = queues.context(channel).await?;
            let locale = app.locale(&context.channel.settings);
            let player = sub
                .user("player")
                .ok_or_else(|| ServiceError::Rejected("no player was given".to_string()))?;
            let snapshot = queues.force_remove(&context, actor, player).await?;
            Ok(Reply::public(locale.format(
                "queue.left",
                &[
                    ("user", &format!("<@{player}>")),
                    ("current", &snapshot.len().to_string()),
                    ("size", &context.queue.settings.size.to_string()),
                ],
            )))
        }
        "clear" => {
            let context = queues.context(channel).await?;
            let locale = app.locale(&context.channel.settings);
            queues.clear(&context, actor).await?;
            Ok(Reply::public(locale.get("queue.cleared")))
        }
        "start" => {
            let context = queues.context(channel).await?;
            let locale = app.locale(&context.channel.settings);
            let started = queues.start_match(&context, Some(actor)).await?;
            Ok(match started {
                Some(id) => {
                    Reply::public(locale.format("match.started", &[("id", &id.to_string())]))
                }
                None => Reply::private(locale.get("queue.empty")),
            })
        }
        other => Err(ServiceError::Rejected(format!(
            "unknown subcommand {other}"
        ))),
    }
}

fn queue_patch(opts: &Opts<'_>) -> QueuePatch {
    // `clear-roles` is a single switch that empties every role field, because
    // Discord options cannot express "set this to nothing".
    let clear = opts.boolean("clear-roles").unwrap_or(false);
    let role = |name: &str| -> Option<Option<RoleId>> {
        match opts.role(name) {
            Some(role) => Some(Some(role)),
            None if clear => Some(None),
            None => None,
        }
    };

    QueuePatch {
        name: opts.string("name").map(str::to_string),
        description: opts.string("description").map(str::to_string),
        size: opts.integer("size").map(|value| value as u32),
        team_count: opts.integer("teams").map(|value| value as u32),
        autostart: opts.boolean("autostart"),
        ranked: opts.boolean("ranked"),
        team_formation: opts
            .string("team-formation")
            .and_then(TeamFormationMode::parse),
        captain_mode: opts.string("captains").and_then(CaptainMode::parse),
        pick_order: opts.string("pick-order").map(str::to_string),
        team_names: opts.string("team-names").map(comma_list),
        team_emojis: opts.string("team-emojis").map(comma_list),
        check_in_seconds: opts.integer("check-in"),
        check_in_abort_on_decline: opts.boolean("check-in-abort-on-decline"),
        check_in_return_policy: opts
            .string("check-in-return")
            .and_then(CheckInReturnPolicy::parse),
        maps: opts.string("maps").map(comma_list),
        map_pick_count: opts.integer("map-count").map(|value| value as u32),
        map_cooldown: opts.integer("map-cooldown").map(|value| value as u32),
        map_vote_candidates: opts.integer("map-vote").map(|value| value as u32),
        map_vote_tie_break: opts.string("map-vote-tie-break").map(|value| {
            if value == "deterministic" {
                TieBreak::Deterministic
            } else {
                TieBreak::Random
            }
        }),
        match_lifetime_seconds: opts.integer("match-lifetime"),
        server_text: opts.string("server").map(str::to_string),
        start_message: opts.string("start-message").map(str::to_string),
        promotion_role: role("promotion-role"),
        whitelist_role: role("whitelist-role"),
        blacklist_role: role("blacklist-role"),
        captain_role: role("captain-role"),
        promotion_cooldown_seconds: opts.integer("promotion-cooldown"),
        start_dm: opts.boolean("start-dm"),
        show_streams: opts.boolean("show-streams"),
    }
}

// --------------------------------------------------------------- /match

async fn match_command(
    app: &AppContext,
    channel: ChannelId,
    actor: &Actor,
    opts: &Opts<'_>,
) -> ServiceResult<Reply> {
    let config = app.store.require_enabled_channel(channel).await?;
    app.require_permission(actor, &config.settings, PermissionLevel::Moderator)?;
    let locale = app.locale(&config.settings);
    let matches = MatchService::new(app.clone());
    let (name, sub) = opts
        .subcommand()
        .ok_or_else(|| ServiceError::Rejected("no subcommand given".to_string()))?;

    let match_id = |sub: &Opts<'_>| -> ServiceResult<crate::domain::ids::MatchId> {
        sub.integer("match")
            .map(crate::domain::ids::MatchId)
            .ok_or_else(|| ServiceError::Rejected("no match number was given".to_string()))
    };

    match name {
        "report" => {
            let id = match_id(&sub)?;
            let outcome = parse_moderator_outcome(sub.string("result").unwrap_or("cancel"))?;
            matches
                .moderator_report(id, actor.user, outcome, None)
                .await?;
            Ok(Reply::public(locale.format(
                "report.final",
                &[
                    ("id", &id.to_string()),
                    (
                        "result",
                        &describe_outcome(outcome, &config_settings(app, id).await?, locale),
                    ),
                ],
            )))
        }
        "cancel" => {
            let id = match_id(&sub)?;
            matches.cancel(id, Some(actor.user)).await?;
            Ok(Reply::public(
                locale.format("match.cancelled", &[("id", &id.to_string())]),
            ))
        }
        "sub-player" => {
            let id = match_id(&sub)?;
            let out = sub
                .user("out")
                .ok_or_else(|| ServiceError::Rejected("no outgoing player".to_string()))?;
            let into = sub
                .user("into")
                .ok_or_else(|| ServiceError::Rejected("no incoming player".to_string()))?;
            matches.substitute(id, out, into).await?;
            Ok(Reply::public(locale.format(
                "match.sub_done",
                &[
                    ("into", &format!("<@{into}>")),
                    ("out", &format!("<@{out}>")),
                    ("id", &id.to_string()),
                ],
            )))
        }
        "put" => {
            let id = match_id(&sub)?;
            let player = sub
                .user("player")
                .ok_or_else(|| ServiceError::Rejected("no player was given".to_string()))?;
            let team = sub.integer("team").map(|value| value.max(1) as usize - 1);
            matches.place_player(id, player, team).await?;
            Ok(Reply::private(locale.get("moderation.confirmed")))
        }
        "create" => {
            let team1 = parse_user_list(sub.string("team1").unwrap_or_default())?;
            let team2 = parse_user_list(sub.string("team2").unwrap_or_default())?;
            let outcome = parse_moderator_outcome(sub.string("result").unwrap_or("draw"))?;
            let size = team1.len() + team2.len();
            let settings = historical_settings(2, size);
            let id = matches
                .create_historical(&config, settings, vec![team1, team2], outcome, actor.user)
                .await?;
            Ok(Reply::public(locale.format(
                "report.final",
                &[("id", &id.to_string()), ("result", "recorded")],
            )))
        }
        other => Err(ServiceError::Rejected(format!(
            "unknown subcommand {other}"
        ))),
    }
}

async fn config_settings(
    app: &AppContext,
    id: crate::domain::ids::MatchId,
) -> ServiceResult<crate::domain::settings::QueueSettings> {
    Ok(app.store.require_match(id).await?.info.settings)
}

fn parse_moderator_outcome(raw: &str) -> ServiceResult<ReportOutcome> {
    match raw {
        "draw" => Ok(ReportOutcome::Draw),
        "cancel" => Ok(ReportOutcome::Cancel),
        // Team numbers are 1-based for moderators, matching the embed.
        other => other
            .parse::<usize>()
            .ok()
            .filter(|team| *team >= 1)
            .map(|team| ReportOutcome::Win(team - 1))
            .ok_or_else(|| ServiceError::Rejected(format!("{other:?} is not a valid result"))),
    }
}

// -------------------------------------------------------------- /rating

async fn rating_command(
    app: &AppContext,
    channel: ChannelId,
    actor: &Actor,
    opts: &Opts<'_>,
) -> ServiceResult<Reply> {
    let config = app.store.require_enabled_channel(channel).await?;
    app.require_permission(actor, &config.settings, PermissionLevel::Administrator)?;
    let ratings = RatingService::new(app.clone());
    let (name, sub) = opts
        .subcommand()
        .ok_or_else(|| ServiceError::Rejected("no subcommand given".to_string()))?;
    let player = |sub: &Opts<'_>| -> ServiceResult<UserId> {
        sub.user("player")
            .ok_or_else(|| ServiceError::Rejected("no player was given".to_string()))
    };

    match name {
        "seed" => {
            let user = player(&sub)?;
            let rating = sub
                .number("rating")
                .ok_or_else(|| ServiceError::Rejected("no rating was given".to_string()))?;
            let delta = ratings
                .seed(&config, actor.user, user, rating, sub.number("deviation"))
                .await?;
            Ok(Reply::private(format!(
                "<@{user}>: {:.0} → {:.0}",
                delta.rating_before, delta.rating_after
            )))
        }
        "penalty" => {
            let user = player(&sub)?;
            let amount = sub
                .number("amount")
                .ok_or_else(|| ServiceError::Rejected("no amount was given".to_string()))?;
            let reason = sub.string("reason").unwrap_or("penalty");
            let delta = ratings
                .penalty(&config, actor.user, user, amount, reason)
                .await?;
            Ok(Reply::public(format!(
                "<@{user}>: {:.0} → {:.0} ({reason})",
                delta.rating_before, delta.rating_after
            )))
        }
        "hide" | "unhide" => {
            let user = player(&sub)?;
            ratings
                .set_hidden(&config, actor.user, user, name == "hide")
                .await?;
            Ok(Reply::private(
                app.locale(&config.settings).get("moderation.confirmed"),
            ))
        }
        "snap" => {
            let changed = ratings.snap_to_rank_floors(&config, actor.user).await?;
            Ok(Reply::private(format!("{changed} ratings snapped.")))
        }
        other => Err(ServiceError::Rejected(format!(
            "unknown subcommand {other}"
        ))),
    }
}

// --------------------------------------------------------------- /stats

async fn stats_command(
    app: &AppContext,
    channel: ChannelId,
    actor: &Actor,
    opts: &Opts<'_>,
) -> ServiceResult<Reply> {
    let config = app.store.require_enabled_channel(channel).await?;
    let locale = app.locale(&config.settings);
    let ratings = RatingService::new(app.clone());
    let (name, sub) = opts
        .subcommand()
        .ok_or_else(|| ServiceError::Rejected("no subcommand given".to_string()))?;

    match name {
        "show" => {
            let totals = app.store.channel_totals(channel).await?;
            Ok(Reply::embed(
                CreateEmbed::new()
                    .title(config.settings.locale.to_uppercase())
                    .title("Channel statistics")
                    .field("Completed", totals.completed.to_string(), true)
                    .field("Cancelled", totals.cancelled.to_string(), true)
                    .field("Live", totals.live.to_string(), true)
                    .field("Last 7 days", totals.last_week.to_string(), true)
                    .field("Players", totals.distinct_players.to_string(), true),
            ))
        }
        "reset" => {
            app.require_permission(actor, &config.settings, PermissionLevel::Administrator)?;
            let removed = ratings.reset_channel(&config, actor.user).await?;
            Ok(Reply::public(format!("{removed} rating records deleted.")))
        }
        "reset-player" => {
            app.require_permission(actor, &config.settings, PermissionLevel::Administrator)?;
            let user = sub
                .user("player")
                .ok_or_else(|| ServiceError::Rejected("no player was given".to_string()))?;
            ratings.reset_player(&config, actor.user, user).await?;
            Ok(Reply::private(locale.get("moderation.confirmed")))
        }
        "replace-player" => {
            app.require_permission(actor, &config.settings, PermissionLevel::Administrator)?;
            let from = sub
                .user("from")
                .ok_or_else(|| ServiceError::Rejected("no source player".to_string()))?;
            let into = sub
                .user("into")
                .ok_or_else(|| ServiceError::Rejected("no target player".to_string()))?;
            ratings
                .replace_player(&config, actor.user, from, into)
                .await?;
            Ok(Reply::private(locale.get("moderation.confirmed")))
        }
        other => Err(ServiceError::Rejected(format!(
            "unknown subcommand {other}"
        ))),
    }
}

// -------------------------------------------------------------- /noadds

async fn noadds_command(
    app: &AppContext,
    guild: GuildId,
    channel: ChannelId,
    actor: &Actor,
    opts: &Opts<'_>,
) -> ServiceResult<Reply> {
    let config = app.store.require_enabled_channel(channel).await?;
    let locale = app.locale(&config.settings);
    let moderation = ModerationService::new(app.clone());
    let (name, sub) = opts
        .subcommand()
        .ok_or_else(|| ServiceError::Rejected("no subcommand given".to_string()))?;

    match name {
        "list" => {
            let bans = moderation.list_bans(guild).await?;
            if bans.is_empty() {
                return Ok(Reply::private(locale.get("moderation.no_bans")));
            }
            let body = bans
                .iter()
                .map(|ban| {
                    format!(
                        "<@{}> — until <t:{}:R>{}",
                        ban.user,
                        ban.expires_at.timestamp(),
                        ban.reason
                            .as_ref()
                            .map_or_else(String::new, |reason| format!(" ({reason})"))
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            Ok(Reply::private_embed(
                CreateEmbed::new().title("Queue bans").description(body),
            ))
        }
        "add" => {
            let user = sub
                .user("player")
                .ok_or_else(|| ServiceError::Rejected("no player was given".to_string()))?;
            let seconds = sub.integer("seconds").unwrap_or(0);
            moderation
                .ban(&config, actor, user, seconds, sub.string("reason"))
                .await?;
            Ok(Reply::public(locale.format(
                "moderation.banned",
                &[
                    ("user", &format!("<@{user}>")),
                    ("duration", &humanize_seconds(seconds)),
                ],
            )))
        }
        "remove" => {
            let user = sub
                .user("player")
                .ok_or_else(|| ServiceError::Rejected("no player was given".to_string()))?;
            moderation.unban(&config, actor, user).await?;
            Ok(Reply::public(locale.format(
                "moderation.unbanned",
                &[("user", &format!("<@{user}>"))],
            )))
        }
        other => Err(ServiceError::Rejected(format!(
            "unknown subcommand {other}"
        ))),
    }
}

// ------------------------------------------------------------- /phrases

async fn phrases_command(
    app: &AppContext,
    channel: ChannelId,
    actor: &Actor,
    opts: &Opts<'_>,
) -> ServiceResult<Reply> {
    let config = app.store.require_enabled_channel(channel).await?;
    let locale = app.locale(&config.settings);
    let moderation = ModerationService::new(app.clone());
    let (name, sub) = opts
        .subcommand()
        .ok_or_else(|| ServiceError::Rejected("no subcommand given".to_string()))?;
    let user = sub
        .user("player")
        .ok_or_else(|| ServiceError::Rejected("no player was given".to_string()))?;

    match name {
        "add" => {
            let phrase = sub
                .string("phrase")
                .ok_or_else(|| ServiceError::Rejected("no phrase was given".to_string()))?;
            moderation.add_phrase(&config, actor, user, phrase).await?;
            Ok(Reply::private(locale.get("moderation.confirmed")))
        }
        "clear" => {
            let removed = moderation.clear_phrases(&config, actor, user).await?;
            Ok(Reply::private(format!("{removed} phrases removed.")))
        }
        other => Err(ServiceError::Rejected(format!(
            "unknown subcommand {other}"
        ))),
    }
}

// ---------------------------------------------------------- components

/// Handles one button press, returning what to reply.
///
/// # Errors
///
/// Returns [`ServiceError::Rejected`] for an identifier this build does not
/// recognise, otherwise whatever the underlying service returned.
pub async fn handle_component(
    app: &AppContext,
    interaction: &ComponentInteraction,
) -> ServiceResult<Reply> {
    let Some(guild_id) = interaction.guild_id else {
        return Err(ServiceError::Rejected(
            "PUGbot components only work inside a server".to_string(),
        ));
    };
    app.ensure_guild_allowed(GuildId::from(guild_id.get()))?;

    let user = UserId::from(interaction.user.id.get());
    let action = custom_id::parse(&interaction.data.custom_id)
        .ok_or_else(|| ServiceError::Rejected("that button is no longer valid".to_string()))?;
    let matches = MatchService::new(app.clone());

    match action {
        custom_id::Action::CheckIn { match_id, ready } => {
            let loaded = app.store.require_match(match_id).await?;
            let config = app
                .store
                .require_enabled_channel(loaded.info.channel)
                .await?;
            let locale = app.locale(&config.settings);
            matches.set_ready(match_id, user, ready).await?;
            Ok(Reply::private(locale.get(if ready {
                "checkin.you_are_ready"
            } else {
                "checkin.you_declined"
            })))
        }
        custom_id::Action::MapVote {
            match_id,
            candidate,
        } => {
            let loaded = app.store.require_match(match_id).await?;
            let config = app
                .store
                .require_enabled_channel(loaded.info.channel)
                .await?;
            let locale = app.locale(&config.settings);
            matches.cast_vote(match_id, user, candidate).await?;
            Ok(Reply::private(locale.get("mapvote.recorded")))
        }
        custom_id::Action::Report { match_id, choice } => {
            let loaded = app.store.require_match(match_id).await?;
            let config = app
                .store
                .require_enabled_channel(loaded.info.channel)
                .await?;
            let locale = app.locale(&config.settings);
            let outcome = if choice == "draw" {
                ReportOutcome::Draw
            } else {
                parse_moderator_outcome(&format!("{}", choice.parse::<usize>().unwrap_or(0) + 1))?
            };
            let status = matches.report(match_id, user, outcome).await?;
            Ok(match status {
                ReportStatus::Pending => Reply::private(locale.get("report.recorded")),
                ReportStatus::Disputed => Reply::public(
                    locale.format("report.disputed", &[("id", &match_id.to_string())]),
                ),
                ReportStatus::Final(outcome) => Reply::public(locale.format(
                    "report.final",
                    &[
                        ("id", &match_id.to_string()),
                        (
                            "result",
                            &describe_outcome(outcome, &loaded.info.settings, locale),
                        ),
                    ],
                )),
            })
        }
    }
}

/// Turns an error into the message the user sees.
///
/// This is the boundary that keeps internal detail out of Discord.
///
/// User errors are shown verbatim and ephemerally; internal errors are logged
/// and replaced with a generic message so nothing leaks into a channel.
pub fn error_reply(error: &ServiceError, locale: Locale) -> Reply {
    if error.is_user_error() {
        Reply::private(match error {
            ServiceError::ChannelNotEnabled => locale.get("error.channel_not_enabled").to_string(),
            ServiceError::NoQueue => locale.get("error.no_queue").to_string(),
            ServiceError::QueueExists => locale.get("error.queue_exists").to_string(),
            ServiceError::Forbidden => locale.get("error.forbidden").to_string(),
            ServiceError::NoMatch => locale.get("error.no_match").to_string(),
            ServiceError::DebugOnly => locale.get("error.debug_only").to_string(),
            other => other.to_string(),
        })
    } else {
        tracing::error!(%error, "command failed");
        Reply::private(locale.get("error.internal"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comma_lists_are_trimmed_and_compacted() {
        assert_eq!(
            comma_list(" de_dust2 , de_inferno ,, "),
            vec!["de_dust2".to_string(), "de_inferno".to_string()]
        );
        assert!(comma_list("  ,  ").is_empty());
    }

    #[test]
    fn user_lists_accept_mentions_and_raw_ids() {
        assert_eq!(
            parse_user_list("<@111> 222 <@!333>").unwrap(),
            vec![UserId(111), UserId(222), UserId(333)]
        );
    }

    #[test]
    fn user_lists_reject_nonsense_and_duplicates() {
        assert!(parse_user_list("").is_err());
        assert!(parse_user_list("nobody").is_err());
        assert!(parse_user_list("111 111").is_err());
    }

    #[test]
    fn moderator_results_use_one_based_team_numbers() {
        assert_eq!(parse_moderator_outcome("1").unwrap(), ReportOutcome::Win(0));
        assert_eq!(parse_moderator_outcome("2").unwrap(), ReportOutcome::Win(1));
        assert_eq!(
            parse_moderator_outcome("draw").unwrap(),
            ReportOutcome::Draw
        );
        assert_eq!(
            parse_moderator_outcome("cancel").unwrap(),
            ReportOutcome::Cancel
        );
        assert!(parse_moderator_outcome("0").is_err());
        assert!(parse_moderator_outcome("nonsense").is_err());
    }

    #[test]
    fn internal_errors_never_reach_the_user_verbatim() {
        let error = ServiceError::Other(anyhow::anyhow!("connection string leaked"));
        let reply = error_reply(&error, Locale::fallback());
        let content = reply.content.unwrap();
        assert!(!content.contains("connection string"), "{content}");
        assert!(reply.ephemeral);
    }

    #[test]
    fn user_errors_are_shown_and_stay_private() {
        let reply = error_reply(&ServiceError::NoQueue, Locale::fallback());
        assert!(reply.ephemeral);
        assert!(reply.content.unwrap().contains("/queue create"));
    }

    #[test]
    fn a_rejection_message_is_passed_through() {
        let reply = error_reply(
            &ServiceError::Rejected("the queue is already full".to_string()),
            Locale::fallback(),
        );
        assert_eq!(reply.content.unwrap(), "the queue is already full");
    }

    #[test]
    fn configuration_dumps_are_cut_to_fit_a_discord_message() {
        let long = "x".repeat(5000);
        let fitted = fit_code(&long);
        assert!(fitted.len() <= 1810);
        assert!(fitted.ends_with('…'));
    }
}
