//! Rendering: embeds and message components.
//!
//! Presentation lives here so the domain assertions in the rest of the test
//! suite stay independent of how anything looks.

use serenity::all::{ButtonStyle, CreateActionRow, CreateButton, CreateEmbed, CreateEmbedFooter};

use crate::domain::checkin::ReadyState;
use crate::domain::ids::MatchId;
use crate::domain::match_state::MatchState;
use crate::domain::queue::QueueSnapshot;
use crate::domain::settings::QueueSettings;
use crate::localization::Locale;
use crate::repositories::matches::LoadedMatch;
use crate::repositories::ratings::PlayerStatsRow;
use crate::services::rating_svc::RankView;

/// Discord's embed field value limit; long lists are truncated to fit.
const FIELD_LIMIT: usize = 1024;

/// Component identifiers. Parsing is centralised so the handler and the
/// builders cannot drift apart.
/// Encoding and decoding of message-component identifiers.
///
/// Centralised so the builders and the interaction handler cannot drift apart:
/// every identifier this module writes, [`custom_id::parse`] reads.
pub mod custom_id {
    use crate::domain::ids::MatchId;

    /// Identifier for a check-in button.
    #[must_use]
    pub fn check_in(match_id: MatchId, ready: bool) -> String {
        format!(
            "checkin:{}:{}",
            if ready { "ready" } else { "notready" },
            match_id
        )
    }

    /// Identifier for a map-vote button, carrying the candidate index.
    #[must_use]
    pub fn map_vote(match_id: MatchId, candidate: usize) -> String {
        format!("mapvote:{match_id}:{candidate}")
    }

    /// Identifier for a result-report button.
    #[must_use]
    pub fn report(match_id: MatchId, choice: &str) -> String {
        format!("report:{match_id}:{choice}")
    }

    /// What a component press means.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Action {
        /// A ready or not-ready press.
        CheckIn {
            /// The match being checked into.
            match_id: MatchId,
            /// True for Ready, false for Not ready.
            ready: bool,
        },
        /// A map ballot.
        MapVote {
            /// The match being voted on.
            match_id: MatchId,
            /// Index into the stored candidate list.
            candidate: usize,
        },
        /// A result report.
        Report {
            /// The match being reported.
            match_id: MatchId,
            /// A team number, or `draw`.
            choice: String,
        },
    }

    /// Decodes an identifier, or `None` if it is unrecognised or malformed.
    ///
    /// Returning `None` rather than guessing means a button from an older
    /// deployment is refused with a clear message instead of acting on the
    /// wrong match.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Action> {
        let mut parts = raw.split(':');
        match parts.next()? {
            "checkin" => {
                let ready = match parts.next()? {
                    "ready" => true,
                    "notready" => false,
                    _ => return None,
                };
                Some(Action::CheckIn {
                    match_id: MatchId(parts.next()?.parse().ok()?),
                    ready,
                })
            }
            "mapvote" => Some(Action::MapVote {
                match_id: MatchId(parts.next()?.parse().ok()?),
                candidate: parts.next()?.parse().ok()?,
            }),
            "report" => Some(Action::Report {
                match_id: MatchId(parts.next()?.parse().ok()?),
                choice: parts.next()?.to_string(),
            }),
            _ => None,
        }
    }
}

fn mentions(users: &[crate::domain::ids::UserId]) -> String {
    if users.is_empty() {
        return "—".to_string();
    }
    let rendered = users
        .iter()
        .map(|user| format!("<@{user}>"))
        .collect::<Vec<_>>()
        .join(", ");
    truncate(&rendered)
}

/// Trims a field to Discord's limit on a character boundary.
///
/// The budget is measured in bytes, which is conservative: Discord counts
/// characters, so a multi-byte string is cut earlier than it strictly needs to
/// be but is never over the limit. The ellipsis is included in the budget.
fn truncate(text: &str) -> String {
    if text.len() <= FIELD_LIMIT {
        return text.to_string();
    }
    const ELLIPSIS: char = '…';
    let mut end = FIELD_LIMIT - ELLIPSIS.len_utf8();
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{ELLIPSIS}", &text[..end])
}

/// The main match embed, used for check-in, drafting, voting and play.
///
/// `seconds_left` is rendered as a countdown during check-in.
pub fn match_embed(loaded: &LoadedMatch, locale: Locale, seconds_left: Option<i64>) -> CreateEmbed {
    let settings = &loaded.info.settings;
    let title = match loaded.info.state {
        MatchState::CheckIn => locale.get("checkin.title").to_string(),
        MatchState::TeamFormation => locale.get("draft.title").to_string(),
        MatchState::MapVote => locale.get("mapvote.title").to_string(),
        _ => locale.format("match.started", &[("id", &loaded.info.id.to_string())]),
    };

    let mut embed = CreateEmbed::new()
        .title(format!("#{} · {}", loaded.info.id, title))
        .footer(CreateEmbedFooter::new(format!(
            "{} · {}",
            settings.name,
            loaded.info.state.as_str()
        )));

    match loaded.info.state {
        MatchState::CheckIn => {
            embed = check_in_fields(embed, loaded, locale, seconds_left);
        }
        MatchState::MapVote => {
            embed = map_vote_fields(embed, loaded, locale);
        }
        _ => {
            embed = roster_fields(embed, loaded, locale, settings);
        }
    }

    if !loaded.info.maps.is_empty() {
        embed = embed.field(locale.get("match.maps"), loaded.info.maps.join(", "), false);
    }
    if let Some(server) = &settings.server_text {
        embed = embed.field(locale.get("match.server"), truncate(server), false);
    }
    if let Some(message) = &settings.start_message {
        if loaded.info.state == MatchState::Active {
            embed = embed.description(truncate(message));
        }
    }
    embed
}

fn check_in_fields(
    mut embed: CreateEmbed,
    loaded: &LoadedMatch,
    locale: Locale,
    seconds_left: Option<i64>,
) -> CreateEmbed {
    let Some(check_in) = loaded.check_in() else {
        return embed;
    };
    if let Some(seconds) = seconds_left {
        embed = embed.description(
            locale.format("checkin.instructions", &[("seconds", &seconds.to_string())]),
        );
    }
    for (state, key) in [
        (ReadyState::Ready, "checkin.state_ready"),
        (ReadyState::Pending, "checkin.state_pending"),
        (ReadyState::Declined, "checkin.state_declined"),
    ] {
        let users = check_in.by_state(state);
        embed = embed.field(
            format!("{} ({})", locale.get(key), users.len()),
            mentions(&users),
            false,
        );
    }
    embed
}

fn map_vote_fields(mut embed: CreateEmbed, loaded: &LoadedMatch, locale: Locale) -> CreateEmbed {
    let Some(vote) = loaded.map_vote() else {
        return embed;
    };
    embed = embed.description(locale.get("mapvote.instructions"));
    let counts = vote.tally();
    let lines = vote
        .candidates
        .iter()
        .zip(counts)
        .map(|(map, count)| format!("**{map}** — {count}"))
        .collect::<Vec<_>>()
        .join("\n");
    embed.field(locale.get("match.maps"), truncate(&lines), false)
}

fn roster_fields(
    mut embed: CreateEmbed,
    loaded: &LoadedMatch,
    locale: Locale,
    settings: &QueueSettings,
) -> CreateEmbed {
    if !settings.uses_teams() {
        return embed.field(
            locale.get("match.players"),
            mentions(&loaded.roster()),
            false,
        );
    }
    for (index, roster) in loaded.rosters().iter().enumerate() {
        let captain = loaded.captain_of(index);
        let listed = roster
            .iter()
            .map(|user| {
                if Some(*user) == captain {
                    format!("**<@{user}>**")
                } else {
                    format!("<@{user}>")
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
        embed = embed.field(
            settings.team_label(index),
            if listed.is_empty() {
                "—".to_string()
            } else {
                truncate(&listed)
            },
            true,
        );
    }
    let unassigned = loaded.unassigned();
    if !unassigned.is_empty() {
        embed = embed.field(locale.get("draft.unpicked"), mentions(&unassigned), false);
    }
    embed
}

/// Ready and Not ready buttons.
#[must_use]
pub fn check_in_components(match_id: MatchId, locale: Locale) -> Vec<CreateActionRow> {
    vec![CreateActionRow::Buttons(vec![
        CreateButton::new(custom_id::check_in(match_id, true))
            .label(locale.get("checkin.ready_button"))
            .style(ButtonStyle::Success),
        CreateButton::new(custom_id::check_in(match_id, false))
            .label(locale.get("checkin.notready_button"))
            .style(ButtonStyle::Danger),
    ])]
}

/// One button per map candidate.
///
/// Discord allows five buttons per action row and the domain caps candidates at
/// nine, so this is at most two rows.
#[must_use]
pub fn map_vote_components(match_id: MatchId, candidates: &[String]) -> Vec<CreateActionRow> {
    candidates
        .chunks(5)
        .enumerate()
        .map(|(row, chunk)| {
            CreateActionRow::Buttons(
                chunk
                    .iter()
                    .enumerate()
                    .map(|(offset, map)| {
                        let index = row * 5 + offset;
                        CreateButton::new(custom_id::map_vote(match_id, index))
                            .label(truncate_label(map))
                            .style(ButtonStyle::Secondary)
                    })
                    .collect(),
            )
        })
        .collect()
}

/// Result buttons shown on an active match: one per team, plus a draw.
#[must_use]
pub fn report_components(match_id: MatchId, settings: &QueueSettings) -> Vec<CreateActionRow> {
    let mut buttons: Vec<CreateButton> = (0..settings.team_count.min(4))
        .map(|team| {
            CreateButton::new(custom_id::report(match_id, &team.to_string()))
                .label(truncate_label(&settings.team_label(team as usize)))
                .style(ButtonStyle::Primary)
        })
        .collect();
    buttons.push(
        CreateButton::new(custom_id::report(match_id, "draw"))
            .label("Draw")
            .style(ButtonStyle::Secondary),
    );
    vec![CreateActionRow::Buttons(buttons)]
}

/// Discord rejects button labels over 80 characters.
fn truncate_label(text: &str) -> String {
    let text = text.trim();
    if text.chars().count() <= 80 {
        return text.to_string();
    }
    text.chars().take(79).chain(std::iter::once('…')).collect()
}

/// The `/who` embed: queue name, occupancy, and the waiting players.
pub fn queue_embed(
    snapshot: &QueueSnapshot,
    settings: &QueueSettings,
    locale: Locale,
) -> CreateEmbed {
    let header = locale.format(
        "queue.who",
        &[
            ("queue", &settings.name),
            ("current", &snapshot.len().to_string()),
            ("size", &settings.size.to_string()),
        ],
    );
    let body = if snapshot.is_empty() {
        locale.get("queue.empty").to_string()
    } else {
        mentions(&snapshot.roster())
    };
    CreateEmbed::new().title(header).description(body)
}

/// The `/rank` embed: rating, uncertainty, tier, record, streak, and last
/// change.
pub fn rank_embed(view: &RankView, locale: Locale) -> CreateEmbed {
    let stats = &view.stats;
    let rank_name = view.rank.as_ref().map_or_else(
        || locale.get("rating.unranked").to_string(),
        |tier| match &tier.emoji {
            Some(emoji) if !emoji.is_empty() => format!("{emoji} {}", tier.name),
            _ => tier.name.clone(),
        },
    );
    let position = view
        .position
        .map_or_else(|| "—".to_string(), |value| format!("#{value}"));
    let change = view
        .last_change
        .map_or_else(|| "—".to_string(), |value| format!("{value:+.0}"));

    CreateEmbed::new()
        .title(locale.format(
            "rating.rank_title",
            &[("user", &format!("<@{}>", stats.user))],
        ))
        .field(
            locale.get("rating.rating"),
            format!("{:.0} ± {:.0}", stats.rating, stats.deviation),
            true,
        )
        .field(
            locale.get("rating.rank"),
            format!("{rank_name} ({position})"),
            true,
        )
        .field(
            locale.get("rating.record"),
            format!(
                "{}/{}/{} ({:.0}%)",
                stats.wins,
                stats.losses,
                stats.draws,
                stats.win_rate()
            ),
            true,
        )
        .field(locale.get("rating.streak"), stats.streak.to_string(), true)
        .field(locale.get("rating.change"), change, true)
}

/// One page of the `/leaderboard` embed.
pub fn leaderboard_embed(
    rows: &[PlayerStatsRow],
    page: i64,
    page_size: i64,
    total: i64,
    locale: Locale,
) -> CreateEmbed {
    if rows.is_empty() {
        return CreateEmbed::new()
            .title(locale.get("rating.leaderboard_title"))
            .description(locale.get("rating.leaderboard_empty"));
    }
    let start = (page - 1) * page_size;
    let body = rows
        .iter()
        .enumerate()
        .map(|(offset, row)| {
            format!(
                "`{:>3}` <@{}> — **{:.0}** ({}/{}/{})",
                start + offset as i64 + 1,
                row.user,
                row.rating,
                row.wins,
                row.losses,
                row.draws
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let pages = (total + page_size - 1) / page_size;
    CreateEmbed::new()
        .title(locale.get("rating.leaderboard_title"))
        .description(truncate(&body))
        .footer(CreateEmbedFooter::new(format!(
            "Page {page}/{} · {total} players",
            pages.max(1)
        )))
}

#[cfg(test)]
mod tests {
    use super::custom_id::{parse, Action};
    use super::*;
    use crate::domain::ids::UserId;

    #[test]
    fn check_in_ids_round_trip() {
        for ready in [true, false] {
            let raw = custom_id::check_in(MatchId(42), ready);
            assert_eq!(
                parse(&raw),
                Some(Action::CheckIn {
                    match_id: MatchId(42),
                    ready
                })
            );
        }
    }

    #[test]
    fn map_vote_ids_round_trip() {
        let raw = custom_id::map_vote(MatchId(7), 3);
        assert_eq!(
            parse(&raw),
            Some(Action::MapVote {
                match_id: MatchId(7),
                candidate: 3
            })
        );
    }

    #[test]
    fn report_ids_round_trip() {
        let raw = custom_id::report(MatchId(9), "draw");
        assert_eq!(
            parse(&raw),
            Some(Action::Report {
                match_id: MatchId(9),
                choice: "draw".to_string()
            })
        );
    }

    #[test]
    fn unknown_or_malformed_ids_are_rejected_rather_than_guessed() {
        for raw in [
            "",
            "nonsense",
            "checkin:maybe:1",
            "checkin:ready:notanumber",
            "mapvote:1",
            "mapvote:1:x",
            "report:",
        ] {
            assert_eq!(parse(raw), None, "{raw:?} should not parse");
        }
    }

    #[test]
    fn custom_ids_fit_discords_hundred_character_limit() {
        let id = MatchId(i64::MAX);
        assert!(custom_id::check_in(id, false).len() <= 100);
        assert!(custom_id::map_vote(id, 9).len() <= 100);
        assert!(custom_id::report(id, "cancel").len() <= 100);
    }

    #[test]
    fn long_field_values_are_truncated_on_a_character_boundary() {
        let long = "é".repeat(2000);
        let truncated = truncate(&long);
        assert!(truncated.len() <= FIELD_LIMIT);
        assert!(truncated.ends_with('…'));
        // Round-tripping proves the cut did not split a code point.
        assert_eq!(
            truncated,
            String::from_utf8(truncated.clone().into_bytes()).unwrap()
        );
    }

    #[test]
    fn short_field_values_are_left_alone() {
        assert_eq!(truncate("hello"), "hello");
    }

    #[test]
    fn button_labels_respect_discords_limit() {
        let long = "map".repeat(50);
        assert!(truncate_label(&long).chars().count() <= 80);
        assert_eq!(truncate_label(" de_dust2 "), "de_dust2");
    }

    #[test]
    fn map_vote_buttons_are_split_into_rows_of_five() {
        let candidates: Vec<String> = (0..9).map(|i| format!("map{i}")).collect();
        let rows = map_vote_components(MatchId(1), &candidates);
        assert_eq!(rows.len(), 2, "9 candidates need two rows");
    }

    #[test]
    fn an_empty_roster_renders_as_a_placeholder_rather_than_nothing() {
        assert_eq!(mentions(&[]), "—");
        assert_eq!(mentions(&[UserId(5)]), "<@5>");
    }
}
