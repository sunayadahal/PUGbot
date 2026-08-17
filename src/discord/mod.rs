//! The Discord adapter: gateway wiring, command registration, and the
//! notifier that background work uses to reach a channel.

pub mod commands;
pub mod embeds;
pub mod handlers;

use std::sync::Arc;

use async_trait::async_trait;
use serenity::all::{
    Command, Context, CreateInteractionResponse, CreateInteractionResponseMessage, CreateMessage,
    EventHandler, GatewayIntents, GuildId as SerenityGuildId, Interaction, OnlineStatus, Presence,
    Ready, UserId as SerenityUserId,
};
use serenity::http::Http;
use serenity::Client;

use crate::config::{AppConfig, Mode};
use crate::domain::ids::{ChannelId, UserId};
use crate::domain::match_state::MatchState;
use crate::error::ServiceResult;
use crate::localization::Locale;
use crate::repositories::Store;
use crate::services::queue_svc::QueueService;
use crate::services::{Announcement, AppContext, Notifier};

/// Sends announcements and DMs that did not originate from an interaction.
#[derive(Clone)]
pub struct DiscordNotifier {
    http: Arc<Http>,
    store: Arc<Store>,
}

impl std::fmt::Debug for DiscordNotifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiscordNotifier").finish_non_exhaustive()
    }
}

impl DiscordNotifier {
    /// Builds a notifier over an HTTP client and the store.
    ///
    /// The HTTP client is independent of the gateway client, so the notifier
    /// exists before the gateway connects.
    #[must_use]
    pub fn new(http: Arc<Http>, store: Arc<Store>) -> Self {
        Self { http, store }
    }

    /// Renders and posts the current state of a match.
    async fn post_match(&self, channel: ChannelId, match_id: crate::domain::ids::MatchId) {
        let Ok(Some(loaded)) = self.store.load_match(match_id).await else {
            return;
        };
        let locale = match self.store.channel_config(loaded.info.channel).await {
            Ok(Some(config)) => Locale::resolve(&config.settings.locale),
            _ => Locale::fallback(),
        };

        let seconds_left = loaded
            .info
            .check_in_ends_at
            .map(|at| (at - chrono::Utc::now()).num_seconds().max(0));
        let embed = embeds::match_embed(&loaded, locale, seconds_left);

        let components = match loaded.info.state {
            MatchState::CheckIn => embeds::check_in_components(match_id, locale),
            MatchState::MapVote => {
                embeds::map_vote_components(match_id, &loaded.info.map_candidates)
            }
            MatchState::Active => embeds::report_components(match_id, &loaded.info.settings),
            _ => Vec::new(),
        };

        let mut message = CreateMessage::new().embed(embed);
        if !components.is_empty() {
            message = message.components(components);
        }
        // A roster mention makes the notification actually reach players.
        if matches!(loaded.info.state, MatchState::CheckIn | MatchState::Active) {
            let mentions = loaded
                .roster()
                .iter()
                .map(|user| format!("<@{user}>"))
                .collect::<Vec<_>>()
                .join(" ");
            message = message.content(mentions);
        }

        let target = serenity::all::ChannelId::new(channel.as_u64());
        if let Err(error) = target.send_message(&self.http, message).await {
            tracing::warn!(%channel, %error, "could not post match update");
        }
    }
}

#[async_trait]
impl Notifier for DiscordNotifier {
    async fn announce(&self, channel: ChannelId, announcement: Announcement) {
        match announcement {
            Announcement::Text(text) => {
                let target = serenity::all::ChannelId::new(channel.as_u64());
                // Suppress mass mentions in text the bot echoes; roles and users
                // it mentions deliberately are built as explicit mentions above.
                let message = CreateMessage::new().content(text).allowed_mentions(
                    serenity::all::CreateAllowedMentions::new()
                        .all_users(true)
                        .all_roles(true)
                        .everyone(false),
                );
                if let Err(error) = target.send_message(&self.http, message).await {
                    tracing::warn!(%channel, %error, "could not announce");
                }
            }
            Announcement::MatchUpdate(id) | Announcement::MatchFinished(id, _) => {
                self.post_match(channel, id).await;
            }
        }
    }

    async fn direct_message(&self, user: UserId, text: String) {
        let target = SerenityUserId::new(user.as_u64());
        match target.create_dm_channel(&self.http).await {
            Ok(dm) => {
                if let Err(error) = dm.id.say(&self.http, text).await {
                    // Closed DMs are normal and must not fail a match start.
                    tracing::debug!(%user, %error, "could not deliver DM");
                }
            }
            Err(error) => tracing::debug!(%user, %error, "could not open a DM channel"),
        }
    }
}

struct Handler {
    app: AppContext,
}

impl Handler {
    /// Resolves the locale for an error reply, falling back to English when
    /// the channel is unknown (which is itself often the error).
    async fn locale_for(&self, channel: ChannelId) -> Locale {
        match self.app.store.channel_config(channel).await {
            Ok(Some(config)) => Locale::resolve(&config.settings.locale),
            _ => Locale::fallback(),
        }
    }
}

#[async_trait]
impl EventHandler for Handler {
    async fn ready(&self, ctx: Context, ready: Ready) {
        let config = &self.app.config;
        tracing::info!(
            mode = config.mode.as_str(),
            bot = %ready.user.name,
            guilds = ready.guilds.len(),
            "connected to Discord"
        );

        let definitions = commands::all();
        match config.mode {
            // Guild-scoped commands update instantly, which is what makes the
            // debug loop usable; global commands can take an hour to propagate.
            Mode::Debug => {
                for guild in &config.guild_allowlist {
                    let target = SerenityGuildId::new(guild.as_u64());
                    match target.set_commands(&ctx.http, definitions.clone()).await {
                        Ok(registered) => tracing::info!(
                            %guild,
                            count = registered.len(),
                            "registered guild commands"
                        ),
                        Err(error) => {
                            tracing::error!(%guild, %error, "failed to register guild commands");
                        }
                    }
                }
            }
            Mode::Production => match Command::set_global_commands(&ctx.http, definitions).await {
                Ok(registered) => {
                    tracing::info!(count = registered.len(), "registered global commands");
                }
                Err(error) => tracing::error!(%error, "failed to register global commands"),
            },
        }
    }

    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        match interaction {
            Interaction::Command(command) => {
                let channel = ChannelId::from(command.channel_id.get());
                let span = tracing::info_span!(
                    "command",
                    mode = self.app.mode(),
                    name = %command.data.name,
                    guild = ?command.guild_id.map(|g| g.get()),
                    channel = %channel,
                    actor = %command.user.id,
                );
                let _entered = span.enter();

                let result = handlers::handle_command(&self.app, &command).await;
                drop(_entered);

                let reply = match result {
                    Ok(reply) => reply,
                    Err(error) => handlers::error_reply(&error, self.locale_for(channel).await),
                };
                respond(&ctx, &command, reply).await;
            }
            Interaction::Component(component) => {
                let channel = ChannelId::from(component.channel_id.get());
                let result = handlers::handle_component(&self.app, &component).await;
                let reply = match result {
                    Ok(reply) => reply,
                    Err(error) => handlers::error_reply(&error, self.locale_for(channel).await),
                };
                let mut message =
                    CreateInteractionResponseMessage::new().ephemeral(reply.ephemeral);
                if let Some(content) = reply.content {
                    message = message.content(content);
                }
                if !reply.embeds.is_empty() {
                    message = message.embeds(reply.embeds);
                }
                if let Err(error) = component
                    .create_response(&ctx.http, CreateInteractionResponse::Message(message))
                    .await
                {
                    tracing::warn!(%error, "could not respond to a component interaction");
                }
            }
            _ => {}
        }
    }

    /// Presence-based queue removal.
    ///
    /// Requires the privileged `GUILD_PRESENCES` intent; without it Discord
    /// never sends this event and the feature is simply inert.
    async fn presence_update(&self, _ctx: Context, presence: Presence) {
        if presence.status != OnlineStatus::Offline && presence.status != OnlineStatus::Idle {
            return;
        }
        let user = UserId::from(presence.user.id.get());
        let is_offline = presence.status == OnlineStatus::Offline;
        let is_afk = presence.status == OnlineStatus::Idle;

        let queues = match self.app.store.queues_for_member(user).await {
            Ok(queues) => queues,
            Err(error) => {
                tracing::warn!(%user, %error, "could not check queues for a presence update");
                return;
            }
        };
        if queues.is_empty() {
            return;
        }

        let prefs = self.app.store.user_prefs(user).await.ok();
        let now = self.app.now();
        for queue_id in queues {
            let Ok(Some(config)) = self.channel_for_queue(queue_id).await else {
                continue;
            };
            let should_remove = crate::domain::queue::should_remove_for_presence(
                &config.settings,
                is_offline,
                is_afk,
                prefs.as_ref().and_then(|p| p.allow_offline_until),
                now,
            );
            if !should_remove {
                continue;
            }
            if let Err(error) = self.app.store.remove_queue_member(queue_id, user).await {
                tracing::warn!(%user, %error, "could not remove an offline player");
                continue;
            }
            let locale = Locale::resolve(&config.settings.locale);
            QueueService::new(self.app.clone())
                .announce(
                    config.channel,
                    format!("<@{user}> — {}", locale.get("queue.presence_removed")),
                )
                .await;
        }
    }
}

impl Handler {
    async fn channel_for_queue(
        &self,
        queue: crate::domain::ids::QueueId,
    ) -> ServiceResult<Option<crate::repositories::ChannelConfigRow>> {
        let channel: Option<i64> =
            sqlx::query_scalar("SELECT channel_id FROM queues WHERE queue_id = $1")
                .bind(queue.get())
                .fetch_optional(self.app.store.pool())
                .await?;
        match channel {
            Some(id) => self.app.store.channel_config(ChannelId(id)).await,
            None => Ok(None),
        }
    }
}

async fn respond(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
    reply: handlers::Reply,
) {
    let mut message = CreateInteractionResponseMessage::new().ephemeral(reply.ephemeral);
    if let Some(content) = reply.content {
        message = message.content(content);
    }
    if !reply.embeds.is_empty() {
        message = message.embeds(reply.embeds);
    }
    if !reply.components.is_empty() {
        message = message.components(reply.components);
    }
    message = message.allowed_mentions(
        serenity::all::CreateAllowedMentions::new()
            .all_users(true)
            .all_roles(true)
            .everyone(false),
    );
    if let Err(error) = command
        .create_response(&ctx.http, CreateInteractionResponse::Message(message))
        .await
    {
        tracing::warn!(%error, "could not respond to a command");
    }
}

/// The gateway intents the enabled features need.
///
/// `GUILD_PRESENCES` is privileged and only requested when some channel has
/// presence-based removal switched on, keeping the bot's permission footprint
/// as small as its configuration allows.
pub fn intents(needs_presences: bool) -> GatewayIntents {
    let mut intents = GatewayIntents::GUILDS | GatewayIntents::GUILD_MEMBERS;
    if needs_presences {
        intents |= GatewayIntents::GUILD_PRESENCES;
    }
    intents
}

/// Builds a Discord client. The caller drives it with `start()`.
///
/// The gateway intents requested depend on configuration: the privileged
/// presence intent is asked for only when some channel actually uses
/// presence-based queue removal, so the bot's permission footprint stays as
/// small as its configuration allows.
///
/// # Errors
///
/// Returns [`ServiceError::Database`](crate::error::ServiceError::Database) if
/// the enabled channels cannot be read, or
/// [`ServiceError::Other`](crate::error::ServiceError::Other) if the client
/// cannot be constructed — usually a malformed token.
pub async fn build_client(app: AppContext, config: &AppConfig) -> ServiceResult<Client> {
    // Requesting a privileged intent that no channel uses would make the bot
    // fail to start on servers that have not granted it.
    let needs_presences = app
        .store
        .enabled_channels()
        .await?
        .iter()
        .any(|channel| channel.settings.remove_offline || channel.settings.remove_afk);

    if needs_presences {
        tracing::info!("requesting the privileged GUILD_PRESENCES intent");
    }

    let client = Client::builder(config.discord_token.expose(), intents(needs_presences))
        .event_handler(Handler { app })
        .await
        .map_err(|error| crate::error::ServiceError::Other(error.into()))?;
    Ok(client)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_privileged_presence_intent_is_only_requested_when_needed() {
        assert!(!intents(false).contains(GatewayIntents::GUILD_PRESENCES));
        assert!(intents(true).contains(GatewayIntents::GUILD_PRESENCES));
    }

    #[test]
    fn guild_scope_is_always_requested() {
        for needs in [true, false] {
            assert!(intents(needs).contains(GatewayIntents::GUILDS));
        }
    }
}
