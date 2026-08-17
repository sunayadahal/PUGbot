//! Slash command definitions.
//!
//! Slash commands are the primary interface; there is no message-prefix
//! fallback. Names use hyphens where Discord allows them, matching the command
//! surface in the specification.

use serenity::all::{CommandOptionType, CreateCommand, CreateCommandOption};

use crate::domain::rating::RatingSystemKind;
use crate::localization::available_locales;

fn opt(kind: CommandOptionType, name: &str, description: &str) -> CreateCommandOption {
    CreateCommandOption::new(kind, name, description)
}

fn required(kind: CommandOptionType, name: &str, description: &str) -> CreateCommandOption {
    opt(kind, name, description).required(true)
}

fn user_opt(name: &str, description: &str) -> CreateCommandOption {
    opt(CommandOptionType::User, name, description)
}

fn sub(name: &str, description: &str) -> CreateCommandOption {
    opt(CommandOptionType::SubCommand, name, description)
}

/// Core queue options, offered by `/queue create` and `/queue set-basics`.
///
/// Discord allows at most 25 options per subcommand, and the full queue
/// configuration has more than that, so editing is split into themed
/// subcommands rather than one unusable wall of options.
fn queue_basic_options() -> Vec<CreateCommandOption> {
    vec![
        opt(
            CommandOptionType::String,
            "name",
            "Display name for the queue",
        ),
        opt(CommandOptionType::String, "description", "Shown by /help"),
        opt(
            CommandOptionType::Integer,
            "size",
            "How many players launch a match",
        )
        .min_int_value(2)
        .max_int_value(100),
        opt(CommandOptionType::Integer, "teams", "Number of teams")
            .min_int_value(2)
            .max_int_value(8),
        opt(
            CommandOptionType::Boolean,
            "autostart",
            "Start automatically when full",
        ),
        opt(
            CommandOptionType::Boolean,
            "ranked",
            "Update ratings from results",
        ),
        opt(
            CommandOptionType::Integer,
            "match-lifetime",
            "Seconds before an unreported match expires",
        )
        .min_int_value(60),
        opt(
            CommandOptionType::String,
            "server",
            "Server details shown at match start",
        ),
        opt(
            CommandOptionType::String,
            "start-message",
            "Extra text shown at match start",
        ),
        opt(
            CommandOptionType::Boolean,
            "start-dm",
            "Send match-start DMs",
        ),
        opt(
            CommandOptionType::Boolean,
            "show-streams",
            "List streaming players",
        ),
    ]
}

/// Team formation and check-in options.
fn queue_team_options() -> Vec<CreateCommandOption> {
    vec![
        opt(
            CommandOptionType::String,
            "team-formation",
            "How teams are built",
        )
        .add_string_choice("Captain draft", "captain_draft")
        .add_string_choice("Rating matchmaking", "rating_matchmaking")
        .add_string_choice("Random teams", "random_teams")
        .add_string_choice("No teams", "no_teams"),
        opt(
            CommandOptionType::String,
            "captains",
            "How captains are chosen",
        )
        .add_string_choice("Captain role, then rating", "role_and_rating")
        .add_string_choice("Closest ratings", "fair_pair")
        .add_string_choice("Random, prefer captain role", "random_with_role_preference")
        .add_string_choice("Random", "random")
        .add_string_choice("Volunteers only", "volunteer"),
        opt(
            CommandOptionType::String,
            "pick-order",
            "Draft order, e.g. ABBA",
        ),
        opt(
            CommandOptionType::String,
            "team-names",
            "Comma-separated team names",
        ),
        opt(
            CommandOptionType::String,
            "team-emojis",
            "Comma-separated team emojis",
        ),
        opt(
            CommandOptionType::Integer,
            "check-in",
            "Ready-check seconds; 0 disables the ready check",
        )
        .min_int_value(0)
        .max_int_value(3600),
        opt(
            CommandOptionType::Boolean,
            "check-in-abort-on-decline",
            "Abort as soon as somebody declines",
        ),
        opt(
            CommandOptionType::String,
            "check-in-return",
            "Who goes back to the queue when a ready check fails",
        )
        .add_string_choice("Only players who were ready", "ready_only")
        .add_string_choice("Ready and silent players", "ready_and_pending")
        .add_string_choice("Nobody", "none"),
    ]
}

/// Map pool, cooldown, and voting options.
fn queue_map_options() -> Vec<CreateCommandOption> {
    vec![
        opt(
            CommandOptionType::String,
            "maps",
            "Comma-separated map pool",
        ),
        opt(
            CommandOptionType::Integer,
            "map-count",
            "Maps played per match",
        )
        .min_int_value(0)
        .max_int_value(10),
        opt(
            CommandOptionType::Integer,
            "map-cooldown",
            "Avoid maps used in this many recent matches",
        )
        .min_int_value(0)
        .max_int_value(50),
        opt(
            CommandOptionType::Integer,
            "map-vote",
            "Candidates put to a vote; 0 disables voting",
        )
        .min_int_value(0)
        .max_int_value(9),
        opt(
            CommandOptionType::String,
            "map-vote-tie-break",
            "How a tied vote is resolved",
        )
        .add_string_choice("First candidate", "deterministic")
        .add_string_choice("Random", "random"),
    ]
}

/// Access, promotion, and captain role options.
fn queue_role_options() -> Vec<CreateCommandOption> {
    vec![
        opt(
            CommandOptionType::Role,
            "promotion-role",
            "Role pinged by /promote",
        ),
        opt(
            CommandOptionType::Role,
            "whitelist-role",
            "Role required to join",
        ),
        opt(
            CommandOptionType::Role,
            "blacklist-role",
            "Role that blocks joining",
        ),
        opt(
            CommandOptionType::Role,
            "captain-role",
            "Role preferred when picking captains",
        ),
        opt(
            CommandOptionType::Integer,
            "promotion-cooldown",
            "Seconds between promotions",
        )
        .min_int_value(0),
        opt(
            CommandOptionType::Boolean,
            "clear-roles",
            "Clear every configured role",
        ),
    ]
}

/// Every queue setting, across all themed groups. Used by the test that keeps
/// the `set-*` subcommands exhaustive.
#[cfg(test)]
fn queue_settings_options() -> Vec<CreateCommandOption> {
    let mut all = queue_basic_options();
    all.extend(queue_team_options());
    all.extend(queue_map_options());
    all.extend(queue_role_options());
    all
}

fn channel_settings_options() -> Vec<CreateCommandOption> {
    let mut locale = opt(
        CommandOptionType::String,
        "locale",
        "Language for this channel",
    );
    for name in available_locales() {
        locale = locale.add_string_choice(name, name);
    }
    let mut rating_system = opt(
        CommandOptionType::String,
        "rating-system",
        "Rating algorithm",
    );
    for kind in RatingSystemKind::ALL {
        let label = match kind {
            RatingSystemKind::Flat => "Flat",
            RatingSystemKind::Glicko2 => "Glicko-2",
            RatingSystemKind::TrueSkill => "TrueSkill",
        };
        rating_system = rating_system.add_string_choice(label, kind.as_str());
    }

    vec![
        locale,
        opt(
            CommandOptionType::Role,
            "admin-role",
            "Role granted administrator rights",
        ),
        opt(
            CommandOptionType::Role,
            "moderator-role",
            "Role granted moderator rights",
        ),
        opt(
            CommandOptionType::Boolean,
            "remove-offline",
            "Drop queued players who go offline",
        ),
        opt(
            CommandOptionType::Boolean,
            "remove-afk",
            "Drop queued players who go idle",
        ),
        opt(
            CommandOptionType::Boolean,
            "allow-offline-opt-out",
            "Let players use /allow-offline",
        ),
        opt(
            CommandOptionType::Integer,
            "default-expiry",
            "Default queue expiry in seconds",
        )
        .min_int_value(60),
        opt(
            CommandOptionType::Integer,
            "max-expiry",
            "Maximum queue expiry in seconds",
        )
        .min_int_value(60),
        opt(
            CommandOptionType::Integer,
            "max-auto-ready",
            "Maximum auto-ready seconds",
        )
        .min_int_value(0),
        rating_system,
        opt(
            CommandOptionType::Number,
            "initial-rating",
            "Rating for a new player",
        ),
        opt(
            CommandOptionType::Number,
            "initial-deviation",
            "Deviation for a new player",
        ),
        opt(
            CommandOptionType::Number,
            "min-deviation",
            "Lowest deviation a player can reach",
        ),
        opt(
            CommandOptionType::Number,
            "rating-scale",
            "Overall rating change scale",
        ),
        opt(
            CommandOptionType::Number,
            "win-scale",
            "Multiplier applied to wins",
        ),
        opt(
            CommandOptionType::Number,
            "loss-scale",
            "Multiplier applied to losses",
        ),
        opt(
            CommandOptionType::Number,
            "draw-bonus",
            "Rating change applied on a draw",
        ),
        opt(
            CommandOptionType::Number,
            "decay-per-day",
            "Rating lost per inactive day",
        ),
        opt(
            CommandOptionType::Number,
            "deviation-decay-per-day",
            "Deviation regained per inactive day",
        ),
        opt(
            CommandOptionType::Boolean,
            "rank-nicknames",
            "Prefix nicknames with the rank",
        ),
        opt(
            CommandOptionType::Integer,
            "leaderboard-min-matches",
            "Matches needed to appear on the leaderboard",
        )
        .min_int_value(0),
        opt(
            CommandOptionType::Integer,
            "leaderboard-activity-days",
            "Only list players active within this many days; 0 disables the cutoff",
        )
        .min_int_value(0),
        opt(
            CommandOptionType::Channel,
            "rating-pool",
            "Share another channel's rating pool",
        ),
        opt(
            CommandOptionType::String,
            "queue-scope",
            "Where a live match blocks queueing",
        )
        .add_string_choice("This channel only", "channel")
        .add_string_choice("The whole server", "guild"),
    ]
}

/// Every command the bot registers.
pub fn all() -> Vec<CreateCommand> {
    let mut commands = vec![
        CreateCommand::new("add")
            .description("Join this channel's queue")
            .add_option(
                opt(
                    CommandOptionType::Integer,
                    "expire",
                    "Seconds before your slot expires; 0 means never",
                )
                .min_int_value(0),
            ),
        CreateCommand::new("remove").description("Leave this channel's queue"),
        CreateCommand::new("who").description("Show who is in this channel's queue"),
        CreateCommand::new("promote").description("Announce this channel's queue"),
        CreateCommand::new("subscribe").description("Get pinged when this queue is promoted"),
        CreateCommand::new("unsubscribe").description("Stop being pinged for this queue"),
        CreateCommand::new("server").description("Show this queue's server details"),
        CreateCommand::new("maps").description("Show this queue's map pool"),
        CreateCommand::new("map").description("Pick a random map from the pool"),
        CreateCommand::new("ready").description("Mark yourself ready for your check-in"),
        CreateCommand::new("notready").description("Decline your current check-in"),
        CreateCommand::new("teams").description("Show your current match"),
        CreateCommand::new("matches").description("Show active matches in this channel"),
        CreateCommand::new("capme").description("Step down as captain"),
        CreateCommand::new("capfor")
            .description("Claim a captain slot")
            .add_option(
                required(
                    CommandOptionType::Integer,
                    "team",
                    "Team number, starting at 1",
                )
                .min_int_value(1)
                .max_int_value(8),
            ),
        CreateCommand::new("pick")
            .description("Pick a player for your team")
            .add_option(user_opt("player", "The player to pick").required(true)),
        CreateCommand::new("subme").description("Ask for a substitute in your match"),
        CreateCommand::new("subfor")
            .description("Take a player's place in a match")
            .add_option(user_opt("player", "The player you are replacing").required(true)),
        CreateCommand::new("report")
            .description("Report the result of your match")
            .add_option(
                required(CommandOptionType::String, "result", "What happened")
                    .add_string_choice("My team won", "win")
                    .add_string_choice("The other team won", "loss")
                    .add_string_choice("Draw", "draw")
                    .add_string_choice("Cancel the match", "cancel"),
            ),
        CreateCommand::new("rank")
            .description("Show a player's rating and rank")
            .add_option(user_opt("player", "Defaults to you")),
        CreateCommand::new("leaderboard")
            .description("Show this channel's leaderboard")
            .add_option(opt(CommandOptionType::Integer, "page", "Page number").min_int_value(1)),
        CreateCommand::new("top").description("Show the most active players"),
        CreateCommand::new("lastgame")
            .description("Show the most recent finished match")
            .add_option(user_opt("player", "Only matches this player was in")),
        CreateCommand::new("nick").description("Apply your rating nickname prefix"),
        CreateCommand::new("help").description("Explain how this channel's queue works"),
        CreateCommand::new("commands").description("List PUGbot commands"),
        CreateCommand::new("switch-dms").description("Toggle match-start direct messages"),
        CreateCommand::new("expire")
            .description("Change when your current queue slot expires")
            .add_option(
                required(
                    CommandOptionType::Integer,
                    "seconds",
                    "0 means never expire",
                )
                .min_int_value(0),
            ),
        CreateCommand::new("expire-default")
            .description("Set your default queue expiry")
            .add_option(
                required(
                    CommandOptionType::Integer,
                    "seconds",
                    "0 means never expire",
                )
                .min_int_value(0),
            ),
        CreateCommand::new("auto-ready")
            .description("Be marked ready automatically for the next match")
            .add_option(
                required(
                    CommandOptionType::Integer,
                    "seconds",
                    "How long to stay armed",
                )
                .min_int_value(1),
            ),
        CreateCommand::new("allow-offline")
            .description("Stay queued even if you go offline")
            .add_option(
                required(
                    CommandOptionType::Integer,
                    "seconds",
                    "How long to allow it",
                )
                .min_int_value(1),
            ),
    ];

    // ------------------------------------------------------------- /queue
    //
    // Creation offers the core options only; the themed `set-*` subcommands
    // together cover every setting, which is enforced by a test below.
    let mut queue_create = sub("create", "Create this channel's queue");
    for option in queue_basic_options() {
        queue_create = queue_create.add_sub_option(option);
    }
    let group = |name: &str, description: &str, options: Vec<CreateCommandOption>| {
        options
            .into_iter()
            .fold(sub(name, description), CreateCommandOption::add_sub_option)
    };

    commands.push(
        CreateCommand::new("queue")
            .description("Manage this channel's queue")
            .add_option(queue_create)
            .add_option(group(
                "set-basics",
                "Change size, teams, ranked, and match text",
                queue_basic_options(),
            ))
            .add_option(group(
                "set-teams",
                "Change team formation, captains, and check-in",
                queue_team_options(),
            ))
            .add_option(group(
                "set-maps",
                "Change the map pool, cooldown, and voting",
                queue_map_options(),
            ))
            .add_option(group(
                "set-roles",
                "Change access, promotion, and captain roles",
                queue_role_options(),
            ))
            .add_option(sub("show", "Show this channel's queue settings"))
            .add_option(sub("delete", "Delete this channel's queue"))
            .add_option(
                sub("add-player", "Put a player in the queue")
                    .add_sub_option(user_opt("player", "The player").required(true)),
            )
            .add_option(
                sub("remove-player", "Take a player out of the queue")
                    .add_sub_option(user_opt("player", "The player").required(true)),
            )
            .add_option(sub("clear", "Empty the queue"))
            .add_option(sub("start", "Start a match with whoever is queued")),
    );

    // ------------------------------------------------------------- /match
    commands.push(
        CreateCommand::new("match")
            .description("Manage active matches")
            .add_option(
                sub("report", "Force a result for a match")
                    .add_sub_option(
                        required(CommandOptionType::Integer, "match", "Match number")
                            .min_int_value(1),
                    )
                    .add_sub_option(
                        required(CommandOptionType::String, "result", "The result")
                            .add_string_choice("Team 1 won", "1")
                            .add_string_choice("Team 2 won", "2")
                            .add_string_choice("Draw", "draw")
                            .add_string_choice("Cancel", "cancel"),
                    ),
            )
            .add_option(sub("cancel", "Cancel a match").add_sub_option(
                required(CommandOptionType::Integer, "match", "Match number").min_int_value(1),
            ))
            .add_option(
                sub("sub-player", "Swap one player for another")
                    .add_sub_option(
                        required(CommandOptionType::Integer, "match", "Match number")
                            .min_int_value(1),
                    )
                    .add_sub_option(user_opt("out", "The player leaving").required(true))
                    .add_sub_option(user_opt("into", "The player joining").required(true)),
            )
            .add_option(
                sub("put", "Move a player between teams")
                    .add_sub_option(
                        required(CommandOptionType::Integer, "match", "Match number")
                            .min_int_value(1),
                    )
                    .add_sub_option(user_opt("player", "The player").required(true))
                    .add_sub_option(
                        opt(
                            CommandOptionType::Integer,
                            "team",
                            "Team number; omit to move them back to the unpicked pool",
                        )
                        .min_int_value(1)
                        .max_int_value(8),
                    ),
            )
            .add_option(
                sub("create", "Record a match that was played outside the bot")
                    .add_sub_option(required(
                        CommandOptionType::String,
                        "team1",
                        "Team 1 players, mentioned or by ID, separated by spaces",
                    ))
                    .add_sub_option(required(
                        CommandOptionType::String,
                        "team2",
                        "Team 2 players, mentioned or by ID, separated by spaces",
                    ))
                    .add_sub_option(
                        required(CommandOptionType::String, "result", "The result")
                            .add_string_choice("Team 1 won", "1")
                            .add_string_choice("Team 2 won", "2")
                            .add_string_choice("Draw", "draw"),
                    ),
            ),
    );

    // ------------------------------------------------------------ /rating
    commands.push(
        CreateCommand::new("rating")
            .description("Rating administration")
            .add_option(
                sub("seed", "Set a player's rating outright")
                    .add_sub_option(user_opt("player", "The player").required(true))
                    .add_sub_option(required(CommandOptionType::Number, "rating", "New rating"))
                    .add_sub_option(opt(CommandOptionType::Number, "deviation", "New deviation")),
            )
            .add_option(
                sub("penalty", "Add or remove rating with a reason")
                    .add_sub_option(user_opt("player", "The player").required(true))
                    .add_sub_option(required(
                        CommandOptionType::Number,
                        "amount",
                        "Rating change; negative to penalise",
                    ))
                    .add_sub_option(required(CommandOptionType::String, "reason", "Why")),
            )
            .add_option(
                sub("hide", "Hide a player from the leaderboard")
                    .add_sub_option(user_opt("player", "The player").required(true)),
            )
            .add_option(
                sub("unhide", "Show a player on the leaderboard again")
                    .add_sub_option(user_opt("player", "The player").required(true)),
            )
            .add_option(sub("snap", "Snap every rating to its rank floor")),
    );

    // ------------------------------------------------------------- /stats
    commands.push(
        CreateCommand::new("stats")
            .description("Statistics")
            .add_option(sub("show", "Show this channel's totals"))
            .add_option(sub("reset", "Delete every rating in this channel"))
            .add_option(
                sub("reset-player", "Delete one player's rating")
                    .add_sub_option(user_opt("player", "The player").required(true)),
            )
            .add_option(
                sub("replace-player", "Move a rating record to another account")
                    .add_sub_option(user_opt("from", "The old account").required(true))
                    .add_sub_option(user_opt("into", "The new account").required(true)),
            ),
    );

    // ------------------------------------------------------------ /noadds
    commands.push(
        CreateCommand::new("noadds")
            .description("Timed queue bans")
            .add_option(sub("list", "List active queue bans"))
            .add_option(
                sub("add", "Ban a player from queueing")
                    .add_sub_option(user_opt("player", "The player").required(true))
                    .add_sub_option(
                        required(CommandOptionType::Integer, "seconds", "How long")
                            .min_int_value(1),
                    )
                    .add_sub_option(opt(CommandOptionType::String, "reason", "Why")),
            )
            .add_option(
                sub("remove", "Lift a player's queue ban")
                    .add_sub_option(user_opt("player", "The player").required(true)),
            ),
    );

    // ----------------------------------------------------------- /phrases
    commands.push(
        CreateCommand::new("phrases")
            .description("Custom join phrases")
            .add_option(
                sub("add", "Add a join phrase for a player")
                    .add_sub_option(user_opt("player", "The player").required(true))
                    .add_sub_option(required(CommandOptionType::String, "phrase", "The phrase")),
            )
            .add_option(
                sub("clear", "Remove a player's join phrases")
                    .add_sub_option(user_opt("player", "The player").required(true)),
            ),
    );

    // ----------------------------------------------------------- /channel
    let mut channel_set = sub("set", "Change this channel's configuration");
    for option in channel_settings_options() {
        channel_set = channel_set.add_sub_option(option);
    }
    commands.push(
        CreateCommand::new("channel")
            .description("Channel configuration")
            .add_option(sub("enable", "Enable PUGbot in this channel"))
            .add_option(sub("disable", "Disable PUGbot in this channel"))
            .add_option(sub("show", "Show this channel's configuration"))
            .add_option(channel_set),
    );

    commands
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// The command surface promised by the specification's command table.
    const EXPECTED: &[&str] = &[
        "add",
        "remove",
        "who",
        "promote",
        "subscribe",
        "unsubscribe",
        "server",
        "maps",
        "map",
        "ready",
        "notready",
        "teams",
        "matches",
        "capfor",
        "capme",
        "pick",
        "subme",
        "subfor",
        "report",
        "rank",
        "leaderboard",
        "top",
        "lastgame",
        "nick",
        "help",
        "commands",
        "switch-dms",
        "expire",
        "expire-default",
        "auto-ready",
        "allow-offline",
        "queue",
        "match",
        "rating",
        "stats",
        "noadds",
        "phrases",
        "channel",
    ];

    fn names() -> Vec<String> {
        all()
            .iter()
            .map(|command| {
                serde_json::to_value(command).expect("command serialises")["name"]
                    .as_str()
                    .expect("command has a name")
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn every_specified_command_is_registered() {
        let registered: HashSet<String> = names().into_iter().collect();
        let missing: Vec<&&str> = EXPECTED
            .iter()
            .filter(|name| !registered.contains(**name))
            .collect();
        assert!(missing.is_empty(), "missing commands: {missing:?}");
    }

    #[test]
    fn no_command_is_registered_twice() {
        let names = names();
        let unique: HashSet<&String> = names.iter().collect();
        assert_eq!(
            unique.len(),
            names.len(),
            "duplicate command name in {names:?}"
        );
    }

    #[test]
    fn command_names_satisfy_discord_naming_rules() {
        for name in names() {
            assert!(name.len() <= 32, "{name} is too long");
            assert_eq!(name, name.to_lowercase(), "{name} must be lowercase");
            assert!(
                name.chars().all(|c| c.is_ascii_lowercase() || c == '-'),
                "{name} has characters Discord rejects"
            );
        }
    }

    fn queue_subcommand_options(name: &str) -> Vec<String> {
        let queue = all()
            .into_iter()
            .map(|c| serde_json::to_value(&c).unwrap())
            .find(|c| c["name"] == "queue")
            .expect("/queue is registered");
        queue["options"]
            .as_array()
            .unwrap()
            .iter()
            .find(|o| o["name"] == name)
            .and_then(|o| o["options"].as_array())
            .map(|options| {
                options
                    .iter()
                    .map(|o| o["name"].as_str().unwrap().to_string())
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn the_set_subcommands_between_them_cover_every_queue_setting() {
        let mut covered: HashSet<String> = HashSet::new();
        for group in ["set-basics", "set-teams", "set-maps", "set-roles"] {
            let options = queue_subcommand_options(group);
            assert!(!options.is_empty(), "/queue {group} exposes nothing");
            covered.extend(options);
        }
        let expected: HashSet<String> = queue_settings_options()
            .iter()
            .map(|option| {
                serde_json::to_value(option).unwrap()["name"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect();
        let missing: Vec<&String> = expected.difference(&covered).collect();
        assert!(
            missing.is_empty(),
            "no /queue set-* command exposes {missing:?}"
        );
    }

    #[test]
    fn queue_creation_offers_the_core_settings() {
        let options = queue_subcommand_options("create");
        for expected in ["name", "size", "teams", "ranked", "autostart"] {
            assert!(
                options.iter().any(|o| o == expected),
                "/queue create is missing {expected}"
            );
        }
    }

    #[test]
    fn the_themed_option_groups_do_not_overlap() {
        let groups = [
            ("basics", queue_basic_options()),
            ("teams", queue_team_options()),
            ("maps", queue_map_options()),
            ("roles", queue_role_options()),
        ];
        let mut seen: Vec<(String, &str)> = Vec::new();
        for (group, options) in &groups {
            for option in options {
                let name = serde_json::to_value(option).unwrap()["name"]
                    .as_str()
                    .unwrap()
                    .to_string();
                if let Some((_, other)) = seen.iter().find(|(existing, _)| *existing == name) {
                    panic!("option {name} appears in both {other} and {group}");
                }
                seen.push((name, group));
            }
        }
    }

    #[test]
    fn no_command_exceeds_discords_option_limit() {
        for command in all() {
            let value = serde_json::to_value(&command).unwrap();
            let name = value["name"].as_str().unwrap();
            if let Some(options) = value["options"].as_array() {
                assert!(
                    options.len() <= 25,
                    "/{name} declares {} options, over Discord's limit of 25",
                    options.len()
                );
                for option in options {
                    if let Some(nested) = option["options"].as_array() {
                        assert!(
                            nested.len() <= 25,
                            "/{name} {} declares {} options, over the limit",
                            option["name"],
                            nested.len()
                        );
                    }
                }
            }
        }
    }
}
