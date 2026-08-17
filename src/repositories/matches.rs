//! Match persistence: creation, loading, transitions, and finalisation.
//!
//! A match is stored relationally rather than as one blob, so the invariants
//! the specification lists (one live match per player, one rating row per
//! player per match) are enforced by indexes. The domain types (`Draft`,
//! `CheckIn`, `MapVote`, `ReportLedger`) are rebuilt from those rows on load.

use chrono::{DateTime, Utc};
use sqlx::postgres::PgRow;
use sqlx::Row;

use super::{Store, Tx};
use crate::domain::checkin::{CheckIn, ReadyState};
use crate::domain::draft::{Draft, Pick};
use crate::domain::ids::{ChannelId, GuildId, MatchId, QueueId, UserId};
use crate::domain::maps::MapVote;
use crate::domain::match_state::MatchState;
use crate::domain::report::{ReportLedger, ReportOutcome};
use crate::domain::settings::QueueSettings;
use crate::error::{DomainResult, ServiceError, ServiceResult};

/// A row of `matches`.
#[derive(Debug, Clone)]
pub struct MatchRow {
    /// Primary key, and the match number players see.
    pub id: MatchId,
    /// The guild the match belongs to.
    pub guild: GuildId,
    /// The channel the match was launched from.
    pub channel: ChannelId,
    /// The queue that launched it. Null for a manually recorded match, or once
    /// the queue has been deleted.
    pub queue: Option<QueueId>,
    /// Where the match is in its lifecycle.
    pub state: MatchState,
    /// Bumped on every transition; the optimistic-locking token.
    pub version: i32,
    /// Whether the result moves ratings.
    pub ranked: bool,
    /// The channel whose rating rows this match writes to.
    pub rating_pool: ChannelId,
    /// The queue settings as they were at launch. Later edits to the queue must
    /// not change how this match is interpreted.
    pub settings: QueueSettings,
    /// The maps that were put to a vote, if there was one.
    pub map_candidates: Vec<String>,
    /// The maps actually being played.
    pub maps: Vec<String>,
    /// Per-team scores, indexed by team number, if a reporter supplied them.
    pub scores: Option<Vec<i32>>,
    /// The winning team, or null for a draw, cancellation, or unfinished match.
    pub winner_team: Option<i32>,
    /// The recorded outcome: `win`, `draw`, or `cancel`.
    pub outcome: Option<String>,
    /// Whether ratings have been applied. Set exactly once.
    pub rated: bool,
    /// Which mode created this match, so debug data is always identifiable.
    pub mode: String,
    /// When the ready-check closes.
    pub check_in_ends_at: Option<DateTime<Utc>>,
    /// When the map vote closes.
    pub vote_ends_at: Option<DateTime<Utc>>,
    /// When an unreported match is abandoned as expired.
    pub expires_at: Option<DateTime<Utc>>,
    /// When the match was created.
    pub created_at: DateTime<Utc>,
    /// When it became active.
    pub started_at: Option<DateTime<Utc>>,
    /// When it reached a terminal state.
    pub finished_at: Option<DateTime<Utc>>,
}

impl MatchRow {
    fn from_row(row: &PgRow) -> ServiceResult<Self> {
        let state_raw: String = row.get("state");
        let state = MatchState::parse(&state_raw).ok_or_else(|| {
            ServiceError::Other(anyhow::anyhow!(
                "unknown match state {state_raw:?} in database"
            ))
        })?;
        Ok(Self {
            id: MatchId(row.get("match_id")),
            guild: GuildId(row.get("guild_id")),
            channel: ChannelId(row.get("channel_id")),
            queue: row.get::<Option<i64>, _>("queue_id").map(QueueId),
            state,
            version: row.get("version"),
            ranked: row.get("ranked"),
            rating_pool: ChannelId(row.get("rating_pool_channel_id")),
            settings: serde_json::from_value(row.get("settings"))
                .map_err(|e| ServiceError::Other(e.into()))?,
            map_candidates: serde_json::from_value(row.get("map_candidates"))
                .map_err(|e| ServiceError::Other(e.into()))?,
            maps: serde_json::from_value(row.get("maps"))
                .map_err(|e| ServiceError::Other(e.into()))?,
            scores: row
                .get::<Option<serde_json::Value>, _>("scores")
                .map(serde_json::from_value)
                .transpose()
                .map_err(|e| ServiceError::Other(e.into()))?,
            winner_team: row.get("winner_team"),
            outcome: row.get("outcome"),
            rated: row.get("rated"),
            mode: row.get("mode"),
            check_in_ends_at: row.get("check_in_ends_at"),
            vote_ends_at: row.get("vote_ends_at"),
            expires_at: row.get("expires_at"),
            created_at: row.get("created_at"),
            started_at: row.get("started_at"),
            finished_at: row.get("finished_at"),
        })
    }
}

const MATCH_COLUMNS: &str = "match_id, guild_id, channel_id, queue_id, state, version, ranked, \
     rating_pool_channel_id, settings, map_candidates, maps, scores, winner_team, outcome, \
     rated, mode, check_in_ends_at, vote_ends_at, expires_at, created_at, started_at, finished_at";

/// A row of `match_players`: one player's place in one match.
#[derive(Debug, Clone)]
pub struct MatchPlayerRow {
    /// The player.
    pub user: UserId,
    /// Their team, or `None` while they are unassigned.
    pub team: Option<i32>,
    /// Whether they captain that team.
    pub is_captain: bool,
    /// Their ready-check answer.
    pub ready_state: ReadyState,
    /// The player they replaced, if they came in as a substitute.
    pub substituted_for: Option<UserId>,
    /// Their rating when the match started, snapshotted so the result can be
    /// explained even if their rating moves elsewhere first.
    pub rating_before: Option<f64>,
    /// Their deviation when the match started.
    pub deviation_before: Option<f64>,
}

impl MatchPlayerRow {
    fn from_row(row: &PgRow) -> Self {
        let ready_raw: String = row.get("ready_state");
        Self {
            user: UserId(row.get("user_id")),
            team: row.get("team"),
            is_captain: row.get("is_captain"),
            ready_state: ReadyState::parse(&ready_raw).unwrap_or(ReadyState::Pending),
            substituted_for: row.get::<Option<i64>, _>("substituted_for").map(UserId),
            rating_before: row.get("rating_before"),
            deviation_before: row.get("deviation_before"),
        }
    }
}

/// A row of `draft_picks`.
#[derive(Debug, Clone)]
pub struct DraftPickRow {
    /// Position in the draft, starting at zero.
    pub seq: i32,
    /// The team the player joined.
    pub team: i32,
    /// The captain who picked, or `None` for the automatic final assignment.
    pub captain: Option<UserId>,
    /// The player who was picked.
    pub player: UserId,
}

/// A match plus everything needed to rebuild its domain state.
#[derive(Debug, Clone)]
pub struct LoadedMatch {
    /// The match row itself.
    pub info: MatchRow,
    /// The roster.
    pub players: Vec<MatchPlayerRow>,
    /// Draft picks in order.
    pub picks: Vec<DraftPickRow>,
    /// Map ballots, as `(voter, candidate index)`.
    pub votes: Vec<(UserId, usize)>,
    /// Result reports, as `(reporter, outcome)`.
    pub reports: Vec<(UserId, ReportOutcome)>,
}

impl LoadedMatch {
    /// Every player on the match, including substitutes who have since left.
    #[must_use]
    pub fn roster(&self) -> Vec<UserId> {
        self.players.iter().map(|p| p.user).collect()
    }

    /// Whether this player is on the roster.
    #[must_use]
    pub fn contains(&self, user: UserId) -> bool {
        self.players.iter().any(|p| p.user == user)
    }

    /// Team rosters indexed by team number. Unassigned players are excluded.
    #[must_use]
    pub fn rosters(&self) -> Vec<Vec<UserId>> {
        let team_count = self.info.settings.team_count.max(1) as usize;
        let mut rosters = vec![Vec::new(); team_count];
        for player in &self.players {
            if let Some(team) = player.team {
                if let Some(slot) = rosters.get_mut(team as usize) {
                    slot.push(player.user);
                }
            }
        }
        rosters
    }

    /// Players not yet assigned to a team: the draft pool.
    #[must_use]
    pub fn unassigned(&self) -> Vec<UserId> {
        self.players
            .iter()
            .filter(|p| p.team.is_none())
            .map(|p| p.user)
            .collect()
    }

    /// The captain of a team, if one has been appointed.
    #[must_use]
    pub fn captain_of(&self, team: usize) -> Option<UserId> {
        self.players
            .iter()
            .find(|p| p.is_captain && p.team == Some(team as i32))
            .map(|p| p.user)
    }

    /// Which team a player is on, if they have been assigned one.
    #[must_use]
    pub fn team_of(&self, user: UserId) -> Option<usize> {
        self.players
            .iter()
            .find(|p| p.user == user)
            .and_then(|p| p.team)
            .map(|team| team as usize)
    }

    /// Rebuilds the draft state machine from the stored rows.
    ///
    /// Nothing about a draft is held in memory between commands, so this is how
    /// a restart mid-draft resumes exactly where it left off.
    ///
    /// # Errors
    ///
    /// Returns a [`DomainError`](crate::error::DomainError) if the stored rows cannot form a valid
    /// draft.
    pub fn draft(&self) -> DomainResult<Draft> {
        let settings = &self.info.settings;
        let team_count = settings.team_count as usize;
        let team_size = settings.team_size() as usize;
        let mut teams = vec![Vec::new(); team_count];
        let mut captains = vec![None; team_count];

        // Captains occupy the first roster slot; everyone else follows in pick
        // order so the rebuilt draft matches what players saw.
        for player in &self.players {
            if let Some(team) = player.team.map(|t| t as usize) {
                if team < team_count && player.is_captain {
                    captains[team] = Some(player.user);
                    teams[team].push(player.user);
                }
            }
        }
        for pick in &self.picks {
            let team = pick.team as usize;
            if team < team_count && !teams[team].contains(&pick.player) {
                teams[team].push(pick.player);
            }
        }
        // Anything placed by a moderator override has no pick row.
        for player in &self.players {
            if let Some(team) = player.team.map(|t| t as usize) {
                if team < team_count && !teams[team].contains(&player.user) {
                    teams[team].push(player.user);
                }
            }
        }

        Ok(Draft {
            team_size,
            teams,
            captains,
            pool: self.unassigned(),
            order: settings.pick_order.clone(),
            picks: self
                .picks
                .iter()
                .map(|pick| Pick {
                    seq: pick.seq as usize,
                    team: pick.team as usize,
                    captain: pick.captain,
                    player: pick.player,
                })
                .collect(),
        })
    }

    /// Rebuilds the ready-check, if the match has one.
    #[must_use]
    pub fn check_in(&self) -> Option<CheckIn> {
        let deadline = self.info.check_in_ends_at?;
        Some(CheckIn {
            deadline,
            states: self
                .players
                .iter()
                .map(|p| (p.user, p.ready_state))
                .collect(),
        })
    }

    /// Rebuilds the map vote, if candidates were published.
    #[must_use]
    pub fn map_vote(&self) -> Option<MapVote> {
        if self.info.map_candidates.len() < 2 {
            return None;
        }
        let mut vote = MapVote {
            candidates: self.info.map_candidates.clone(),
            votes: Default::default(),
            eligible: self.roster(),
        };
        for (user, candidate) in &self.votes {
            vote.votes.insert(*user, *candidate);
        }
        Some(vote)
    }

    /// Rebuilds the report ledger, for evaluating consensus.
    #[must_use]
    pub fn report_ledger(&self) -> ReportLedger {
        ReportLedger {
            reports: self.reports.iter().copied().collect(),
            scores: self.info.scores.clone(),
        }
    }
}

/// What a new match needs to exist.
#[derive(Debug, Clone)]
pub struct NewMatch {
    /// The guild the match belongs to.
    pub guild: GuildId,
    /// The channel it is launched from.
    pub channel: ChannelId,
    /// The queue it came from, if any.
    pub queue: Option<QueueId>,
    /// The state to create it in — check-in, or straight to team formation.
    pub state: MatchState,
    /// Whether the result moves ratings.
    pub ranked: bool,
    /// The channel whose rating rows it writes to.
    pub rating_pool: ChannelId,
    /// The queue settings to snapshot against it.
    pub settings: QueueSettings,
    /// The roster.
    pub players: Vec<UserId>,
    /// The running mode, recorded on the row.
    pub mode: String,
    /// When the ready-check should close, if there is one.
    pub check_in_ends_at: Option<DateTime<Utc>>,
    /// When the match should be abandoned if never reported.
    pub expires_at: Option<DateTime<Utc>>,
}

impl Store {
    /// Creates a match and its roster inside `tx`.
    ///
    /// The caller is expected to remove the players from the queue in the same
    /// transaction, so a crash between the two cannot strand them in both.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::AlreadyInMatch`](crate::error::DomainError::AlreadyInMatch) when the
    /// partial unique index
    /// catches a player who is already in a live match in this channel, or
    /// [`ServiceError::Database`] if a query fails.
    pub async fn create_match(&self, tx: &mut Tx<'_>, new: &NewMatch) -> ServiceResult<MatchId> {
        let id: i64 = sqlx::query_scalar(
            "INSERT INTO matches (guild_id, channel_id, queue_id, state, ranked,
                                  rating_pool_channel_id, settings, mode,
                                  check_in_ends_at, expires_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
             RETURNING match_id",
        )
        .bind(new.guild.get())
        .bind(new.channel.get())
        .bind(new.queue.map(QueueId::get))
        .bind(new.state.as_str())
        .bind(new.ranked)
        .bind(new.rating_pool.get())
        .bind(serde_json::to_value(&new.settings).map_err(|e| ServiceError::Other(e.into()))?)
        .bind(&new.mode)
        .bind(new.check_in_ends_at)
        .bind(new.expires_at)
        .fetch_one(&mut **tx)
        .await?;

        for user in &new.players {
            let result = sqlx::query(
                "INSERT INTO match_players (match_id, user_id, channel_id, live)
                 VALUES ($1, $2, $3, TRUE)",
            )
            .bind(id)
            .bind(user.get())
            .bind(new.channel.get())
            .execute(&mut **tx)
            .await;
            if let Err(sqlx::Error::Database(db)) = &result {
                if db.is_unique_violation() {
                    // The partial unique index caught a player who is already
                    // in a live match in this channel.
                    return Err(ServiceError::Domain(
                        crate::error::DomainError::AlreadyInMatch,
                    ));
                }
            }
            result?;
        }

        Ok(MatchId(id))
    }

    /// Loads a match and everything needed to rebuild its domain state.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`] if a query fails.
    pub async fn load_match(&self, id: MatchId) -> ServiceResult<Option<LoadedMatch>> {
        let Some(row) = sqlx::query(&format!(
            "SELECT {MATCH_COLUMNS} FROM matches WHERE match_id = $1"
        ))
        .bind(id.get())
        .fetch_optional(self.pool())
        .await?
        else {
            return Ok(None);
        };
        self.hydrate(row).await.map(Some)
    }

    /// Loads a match, treating absence as an error.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::NoMatch`] if no such match exists.
    pub async fn require_match(&self, id: MatchId) -> ServiceResult<LoadedMatch> {
        self.load_match(id).await?.ok_or(ServiceError::NoMatch)
    }

    async fn hydrate(&self, row: PgRow) -> ServiceResult<LoadedMatch> {
        let info = MatchRow::from_row(&row)?;
        let id = info.id.get();

        let players = sqlx::query(
            "SELECT user_id, team, is_captain, ready_state, substituted_for,
                    rating_before, deviation_before
             FROM match_players WHERE match_id = $1 ORDER BY team NULLS LAST, joined_at, user_id",
        )
        .bind(id)
        .fetch_all(self.pool())
        .await?
        .iter()
        .map(MatchPlayerRow::from_row)
        .collect();

        let picks = sqlx::query(
            "SELECT seq, team, captain_user_id, player_user_id FROM draft_picks
             WHERE match_id = $1 ORDER BY seq",
        )
        .bind(id)
        .fetch_all(self.pool())
        .await?
        .into_iter()
        .map(|row| DraftPickRow {
            seq: row.get("seq"),
            team: row.get("team"),
            captain: row.get::<Option<i64>, _>("captain_user_id").map(UserId),
            player: UserId(row.get("player_user_id")),
        })
        .collect();

        let votes =
            sqlx::query("SELECT user_id, candidate_index FROM map_votes WHERE match_id = $1")
                .bind(id)
                .fetch_all(self.pool())
                .await?
                .into_iter()
                .map(|row| {
                    (
                        UserId(row.get::<i64, _>("user_id")),
                        row.get::<i32, _>("candidate_index") as usize,
                    )
                })
                .collect();

        let reports = sqlx::query(
            "SELECT user_id, outcome, winner_team FROM match_reports WHERE match_id = $1",
        )
        .bind(id)
        .fetch_all(self.pool())
        .await?
        .into_iter()
        .map(|row| {
            let outcome = match row.get::<String, _>("outcome").as_str() {
                "draw" => ReportOutcome::Draw,
                "cancel" => ReportOutcome::Cancel,
                _ => ReportOutcome::Win(
                    row.get::<Option<i32>, _>("winner_team").unwrap_or(0) as usize
                ),
            };
            (UserId(row.get::<i64, _>("user_id")), outcome)
        })
        .collect();

        Ok(LoadedMatch {
            info,
            players,
            picks,
            votes,
            reports,
        })
    }

    /// Every match in a channel that has not reached a terminal state.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`] if a query fails.
    pub async fn live_matches(&self, channel: ChannelId) -> ServiceResult<Vec<LoadedMatch>> {
        let rows = sqlx::query(&format!(
            "SELECT {MATCH_COLUMNS} FROM matches
             WHERE channel_id = $1 AND state NOT IN ('COMPLETED', 'CANCELLED', 'EXPIRED')
             ORDER BY created_at",
        ))
        .bind(channel.get())
        .fetch_all(self.pool())
        .await?;
        let mut matches = Vec::with_capacity(rows.len());
        for row in rows {
            matches.push(self.hydrate(row).await?);
        }
        Ok(matches)
    }

    /// The live match a player is currently in, searched within `scope`.
    /// The live match a player is in.
    ///
    /// Pass `guild` to search the whole server, or `None` to search only
    /// `channel`. This is what implements
    /// [`QueueScope`](crate::domain::settings::QueueScope).
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`] if a query fails.
    pub async fn live_match_for_user(
        &self,
        user: UserId,
        channel: ChannelId,
        guild: Option<GuildId>,
    ) -> ServiceResult<Option<LoadedMatch>> {
        let row = match guild {
            // Guild scope: any live match anywhere in the server blocks them.
            Some(guild) => {
                sqlx::query(&format!(
                    "SELECT {MATCH_COLUMNS} FROM matches m
                     WHERE m.guild_id = $1
                       AND m.state NOT IN ('COMPLETED', 'CANCELLED', 'EXPIRED')
                       AND EXISTS (SELECT 1 FROM match_players p
                                   WHERE p.match_id = m.match_id AND p.user_id = $2 AND p.live)
                     ORDER BY m.created_at LIMIT 1",
                ))
                .bind(guild.get())
                .bind(user.get())
                .fetch_optional(self.pool())
                .await?
            }
            None => {
                sqlx::query(&format!(
                    "SELECT {MATCH_COLUMNS} FROM matches m
                     WHERE m.channel_id = $1
                       AND m.state NOT IN ('COMPLETED', 'CANCELLED', 'EXPIRED')
                       AND EXISTS (SELECT 1 FROM match_players p
                                   WHERE p.match_id = m.match_id AND p.user_id = $2 AND p.live)
                     ORDER BY m.created_at LIMIT 1",
                ))
                .bind(channel.get())
                .bind(user.get())
                .fetch_optional(self.pool())
                .await?
            }
        };
        match row {
            Some(row) => self.hydrate(row).await.map(Some),
            None => Ok(None),
        }
    }

    /// Live matches whose check-in, vote, or lifetime deadline has passed.
    ///
    /// The timer job's work list.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`] if a query fails.
    pub async fn matches_past_deadline(
        &self,
        now: DateTime<Utc>,
    ) -> ServiceResult<Vec<LoadedMatch>> {
        let rows = sqlx::query(&format!(
            "SELECT {MATCH_COLUMNS} FROM matches
             WHERE state NOT IN ('COMPLETED', 'CANCELLED', 'EXPIRED')
               AND ((check_in_ends_at IS NOT NULL AND check_in_ends_at <= $1)
                 OR (vote_ends_at IS NOT NULL AND vote_ends_at <= $1)
                 OR (expires_at IS NOT NULL AND expires_at <= $1))
             ORDER BY created_at",
        ))
        .bind(now)
        .fetch_all(self.pool())
        .await?;
        let mut matches = Vec::with_capacity(rows.len());
        for row in rows {
            matches.push(self.hydrate(row).await?);
        }
        Ok(matches)
    }

    /// Every live match, used to rebuild in-memory timers after a restart.
    /// Every live match, used to report what is being resumed after a restart.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`] if a query fails.
    pub async fn all_live_matches(&self) -> ServiceResult<Vec<LoadedMatch>> {
        let rows = sqlx::query(&format!(
            "SELECT {MATCH_COLUMNS} FROM matches
             WHERE state NOT IN ('COMPLETED', 'CANCELLED', 'EXPIRED') ORDER BY created_at",
        ))
        .fetch_all(self.pool())
        .await?;
        let mut matches = Vec::with_capacity(rows.len());
        for row in rows {
            matches.push(self.hydrate(row).await?);
        }
        Ok(matches)
    }

    /// Applies a state transition under optimistic locking, returning the new
    /// version.
    ///
    /// Moving to a terminal state also clears the `live` flag on every roster
    /// row, which frees those players to queue again and releases the
    /// one-live-match index.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Conflict`] when another handler already moved
    /// the match — which is what makes a double-clicked button safe — or
    /// [`ServiceError::Database`] if a query fails.
    pub async fn transition(
        &self,
        tx: &mut Tx<'_>,
        id: MatchId,
        expected_version: i32,
        to: MatchState,
    ) -> ServiceResult<i32> {
        let version: Option<i32> = sqlx::query_scalar(
            "UPDATE matches
             SET state = $3, version = version + 1, updated_at = now(),
                 started_at = CASE WHEN $3 = 'ACTIVE' AND started_at IS NULL
                                   THEN now() ELSE started_at END,
                 finished_at = CASE WHEN $3 IN ('COMPLETED', 'CANCELLED', 'EXPIRED')
                                    THEN now() ELSE finished_at END
             WHERE match_id = $1 AND version = $2
             RETURNING version",
        )
        .bind(id.get())
        .bind(expected_version)
        .bind(to.as_str())
        .fetch_optional(&mut **tx)
        .await?;

        let version = version.ok_or(ServiceError::Conflict(id))?;

        // Players stop occupying a slot the moment the match ends, which frees
        // them to queue again and releases the one-live-match index.
        if to.is_terminal() {
            sqlx::query("UPDATE match_players SET live = FALSE WHERE match_id = $1")
                .bind(id.get())
                .execute(&mut **tx)
                .await?;
        }
        Ok(version)
    }

    /// Sets or clears the check-in deadline. Clearing it takes the match off the
    /// timer job's work list.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`] if a query fails.
    pub async fn set_check_in_deadline(
        &self,
        tx: &mut Tx<'_>,
        id: MatchId,
        deadline: Option<DateTime<Utc>>,
    ) -> ServiceResult<()> {
        sqlx::query(
            "UPDATE matches SET check_in_ends_at = $2, updated_at = now() WHERE match_id = $1",
        )
        .bind(id.get())
        .bind(deadline)
        .execute(&mut **tx)
        .await?;
        Ok(())
    }

    /// Sets or clears the map-vote deadline.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`] if a query fails.
    pub async fn set_vote_deadline(
        &self,
        tx: &mut Tx<'_>,
        id: MatchId,
        deadline: Option<DateTime<Utc>>,
    ) -> ServiceResult<()> {
        sqlx::query("UPDATE matches SET vote_ends_at = $2, updated_at = now() WHERE match_id = $1")
            .bind(id.get())
            .bind(deadline)
            .execute(&mut **tx)
            .await?;
        Ok(())
    }

    /// Records one player's ready-check answer. Returns whether they were on the
    /// roster.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`] if a query fails.
    pub async fn set_ready_state(
        &self,
        id: MatchId,
        user: UserId,
        state: ReadyState,
    ) -> ServiceResult<bool> {
        let result = sqlx::query(
            "UPDATE match_players SET ready_state = $3 WHERE match_id = $1 AND user_id = $2",
        )
        .bind(id.get())
        .bind(user.get())
        .bind(state.as_str())
        .execute(self.pool())
        .await?;
        Ok(result.rows_affected() > 0)
    }

    /// Writes the whole team assignment at once. Used after balanced/random
    /// formation and after each draft pick.
    /// Replaces the whole team assignment.
    ///
    /// Used after balanced or random formation, and after each draft pick. The
    /// assignment is cleared first, so a player who left a team is not left
    /// behind on it.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`] if a query fails.
    pub async fn set_teams(
        &self,
        tx: &mut Tx<'_>,
        id: MatchId,
        teams: &[Vec<UserId>],
        captains: &[Option<UserId>],
    ) -> ServiceResult<()> {
        sqlx::query("UPDATE match_players SET team = NULL, is_captain = FALSE WHERE match_id = $1")
            .bind(id.get())
            .execute(&mut **tx)
            .await?;
        for (index, roster) in teams.iter().enumerate() {
            for user in roster {
                sqlx::query(
                    "UPDATE match_players SET team = $3, is_captain = $4
                     WHERE match_id = $1 AND user_id = $2",
                )
                .bind(id.get())
                .bind(user.get())
                .bind(index as i32)
                .bind(captains.get(index).copied().flatten() == Some(*user))
                .execute(&mut **tx)
                .await?;
            }
        }
        Ok(())
    }

    /// Appends draft picks to the history. Existing sequence numbers are left
    /// alone, so a retry cannot duplicate a pick.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`] if a query fails.
    pub async fn record_picks(
        &self,
        tx: &mut Tx<'_>,
        id: MatchId,
        picks: &[Pick],
    ) -> ServiceResult<()> {
        for pick in picks {
            sqlx::query(
                "INSERT INTO draft_picks (match_id, seq, team, captain_user_id, player_user_id)
                 VALUES ($1, $2, $3, $4, $5) ON CONFLICT (match_id, seq) DO NOTHING",
            )
            .bind(id.get())
            .bind(pick.seq as i32)
            .bind(pick.team as i32)
            .bind(pick.captain.map(UserId::get))
            .bind(pick.player.get())
            .execute(&mut **tx)
            .await?;
        }
        Ok(())
    }

    /// Records the maps put to a vote, so a historical result can be explained.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`] if a query fails.
    pub async fn set_map_candidates(
        &self,
        tx: &mut Tx<'_>,
        id: MatchId,
        candidates: &[String],
    ) -> ServiceResult<()> {
        sqlx::query(
            "UPDATE matches SET map_candidates = $2, updated_at = now() WHERE match_id = $1",
        )
        .bind(id.get())
        .bind(serde_json::to_value(candidates).map_err(|e| ServiceError::Other(e.into()))?)
        .execute(&mut **tx)
        .await?;
        Ok(())
    }

    /// Records the maps the match will actually play.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`] if a query fails.
    pub async fn set_maps(
        &self,
        tx: &mut Tx<'_>,
        id: MatchId,
        maps: &[String],
    ) -> ServiceResult<()> {
        sqlx::query("UPDATE matches SET maps = $2, updated_at = now() WHERE match_id = $1")
            .bind(id.get())
            .bind(serde_json::to_value(maps).map_err(|e| ServiceError::Other(e.into()))?)
            .execute(&mut **tx)
            .await?;
        Ok(())
    }

    /// Records a ballot, replacing any previous one from the same player.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`] if a query fails.
    pub async fn cast_map_vote(
        &self,
        id: MatchId,
        user: UserId,
        candidate: usize,
    ) -> ServiceResult<()> {
        sqlx::query(
            "INSERT INTO map_votes (match_id, user_id, candidate_index) VALUES ($1, $2, $3)
             ON CONFLICT (match_id, user_id) DO UPDATE SET candidate_index = $3, created_at = now()",
        )
        .bind(id.get())
        .bind(user.get())
        .bind(candidate as i32)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Records one player's result report, replacing any previous one.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`] if a query fails.
    pub async fn record_report(
        &self,
        id: MatchId,
        user: UserId,
        outcome: ReportOutcome,
    ) -> ServiceResult<()> {
        let (label, winner) = match outcome {
            ReportOutcome::Win(team) => ("win", Some(team as i32)),
            ReportOutcome::Draw => ("draw", None),
            ReportOutcome::Cancel => ("cancel", None),
        };
        sqlx::query(
            "INSERT INTO match_reports (match_id, user_id, outcome, winner_team)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (match_id, user_id)
             DO UPDATE SET outcome = $3, winner_team = $4, created_at = now()",
        )
        .bind(id.get())
        .bind(user.get())
        .bind(label)
        .bind(winner)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Records the agreed result on the match row itself.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`] if a query fails.
    pub async fn set_outcome(
        &self,
        tx: &mut Tx<'_>,
        id: MatchId,
        outcome: ReportOutcome,
        scores: Option<&[i32]>,
    ) -> ServiceResult<()> {
        let (label, winner) = match outcome {
            ReportOutcome::Win(team) => ("win", Some(team as i32)),
            ReportOutcome::Draw => ("draw", None),
            ReportOutcome::Cancel => ("cancel", None),
        };
        sqlx::query(
            "UPDATE matches SET outcome = $2, winner_team = $3, scores = $4, updated_at = now()
             WHERE match_id = $1",
        )
        .bind(id.get())
        .bind(label)
        .bind(winner)
        .bind(
            scores
                .map(serde_json::to_value)
                .transpose()
                .map_err(|e| ServiceError::Other(e.into()))?,
        )
        .execute(&mut **tx)
        .await?;
        Ok(())
    }

    /// Claims the one-shot right to rate this match.
    ///
    /// Returns `false` if the flag was already set, which makes rating
    /// application idempotent even if two finalisation paths race past the
    /// state check. The unique index on `rating_history` is the second line of
    /// defence.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`] if the query fails.
    pub async fn claim_rating(&self, tx: &mut Tx<'_>, id: MatchId) -> ServiceResult<bool> {
        let result =
            sqlx::query("UPDATE matches SET rated = TRUE WHERE match_id = $1 AND NOT rated")
                .bind(id.get())
                .execute(&mut **tx)
                .await?;
        Ok(result.rows_affected() > 0)
    }

    /// Replaces `out` with `into` on a match roster.
    ///
    /// The outgoing player keeps a historical row but stops being live, so they
    /// can queue again immediately. The substitute inherits their team and
    /// captaincy.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::NotInMatch`](crate::error::DomainError::NotInMatch) if `out` is not
    /// on the roster,
    /// [`DomainError::AlreadyInMatch`](crate::error::DomainError::AlreadyInMatch) if `into` is
    /// already in a live match in
    /// this channel, or [`ServiceError::Database`] if a query fails.
    pub async fn substitute_player(
        &self,
        tx: &mut Tx<'_>,
        id: MatchId,
        channel: ChannelId,
        out: UserId,
        into: UserId,
    ) -> ServiceResult<()> {
        let row = sqlx::query(
            "SELECT team, is_captain FROM match_players WHERE match_id = $1 AND user_id = $2",
        )
        .bind(id.get())
        .bind(out.get())
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(ServiceError::Domain(crate::error::DomainError::NotInMatch))?;
        let team: Option<i32> = row.get("team");
        let is_captain: bool = row.get("is_captain");

        // The outgoing player keeps a historical row but stops being live, so
        // they can queue again immediately.
        sqlx::query("UPDATE match_players SET live = FALSE WHERE match_id = $1 AND user_id = $2")
            .bind(id.get())
            .bind(out.get())
            .execute(&mut **tx)
            .await?;

        let result = sqlx::query(
            "INSERT INTO match_players (match_id, user_id, channel_id, live, team, is_captain,
                                        substituted_for)
             VALUES ($1, $2, $3, TRUE, $4, $5, $6)",
        )
        .bind(id.get())
        .bind(into.get())
        .bind(channel.get())
        .bind(team)
        .bind(is_captain)
        .bind(out.get())
        .execute(&mut **tx)
        .await;

        if let Err(sqlx::Error::Database(db)) = &result {
            if db.is_unique_violation() {
                return Err(ServiceError::Domain(
                    crate::error::DomainError::AlreadyInMatch,
                ));
            }
        }
        result?;
        Ok(())
    }

    /// Moves a single player between teams, or back to the unassigned pool.
    /// Moves a player between teams, or back to the unassigned pool. Moving them
    /// to the pool also strips any captaincy.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`] if a query fails.
    pub async fn place_player(
        &self,
        tx: &mut Tx<'_>,
        id: MatchId,
        user: UserId,
        team: Option<usize>,
    ) -> ServiceResult<()> {
        sqlx::query(
            "UPDATE match_players SET team = $3, is_captain = CASE WHEN $3 IS NULL THEN FALSE
                                                                   ELSE is_captain END
             WHERE match_id = $1 AND user_id = $2",
        )
        .bind(id.get())
        .bind(user.get())
        .bind(team.map(|t| t as i32))
        .execute(&mut **tx)
        .await?;
        Ok(())
    }

    /// Maps played in this channel's most recent finished matches, newest
    /// first, for cooldown checks.
    /// Maps played in this channel's most recent finished matches, newest first.
    /// Feeds the map cooldown.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`] if a query fails.
    pub async fn recent_maps(&self, channel: ChannelId, limit: i64) -> ServiceResult<Vec<String>> {
        let rows: Vec<serde_json::Value> = sqlx::query_scalar(
            "SELECT maps FROM matches
             WHERE channel_id = $1 AND state = 'COMPLETED' AND jsonb_array_length(maps) > 0
             ORDER BY created_at DESC LIMIT $2",
        )
        .bind(channel.get())
        .bind(limit)
        .fetch_all(self.pool())
        .await?;
        Ok(rows
            .into_iter()
            .filter_map(|value| serde_json::from_value::<Vec<String>>(value).ok())
            .flatten()
            .collect())
    }

    /// Recent finished matches for `/lastgame` and history views.
    /// Recent finished matches, optionally filtered to those a player was in.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`] if a query fails.
    pub async fn recent_matches(
        &self,
        channel: ChannelId,
        user: Option<UserId>,
        limit: i64,
    ) -> ServiceResult<Vec<LoadedMatch>> {
        let rows = match user {
            Some(user) => {
                sqlx::query(&format!(
                    "SELECT {MATCH_COLUMNS} FROM matches m
                     WHERE m.channel_id = $1 AND m.state = 'COMPLETED'
                       AND EXISTS (SELECT 1 FROM match_players p
                                   WHERE p.match_id = m.match_id AND p.user_id = $2)
                     ORDER BY m.finished_at DESC NULLS LAST LIMIT $3",
                ))
                .bind(channel.get())
                .bind(user.get())
                .bind(limit)
                .fetch_all(self.pool())
                .await?
            }
            None => {
                sqlx::query(&format!(
                    "SELECT {MATCH_COLUMNS} FROM matches
                     WHERE channel_id = $1 AND state = 'COMPLETED'
                     ORDER BY finished_at DESC NULLS LAST LIMIT $2",
                ))
                .bind(channel.get())
                .bind(limit)
                .fetch_all(self.pool())
                .await?
            }
        };
        let mut matches = Vec::with_capacity(rows.len());
        for row in rows {
            matches.push(self.hydrate(row).await?);
        }
        Ok(matches)
    }

    /// Channel totals for `/stats show`.
    /// Aggregate counts for `/stats show`.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`] if a query fails.
    pub async fn channel_totals(&self, channel: ChannelId) -> ServiceResult<ChannelTotals> {
        let row = sqlx::query(
            "SELECT
                 count(*) FILTER (WHERE state = 'COMPLETED')  AS completed,
                 count(*) FILTER (WHERE state = 'CANCELLED')  AS cancelled,
                 count(*) FILTER (WHERE state NOT IN ('COMPLETED','CANCELLED','EXPIRED')) AS live,
                 count(*) FILTER (WHERE state = 'COMPLETED'
                                    AND finished_at > now() - interval '7 days') AS last_week
             FROM matches WHERE channel_id = $1",
        )
        .bind(channel.get())
        .fetch_one(self.pool())
        .await?;
        let players: i64 = sqlx::query_scalar(
            "SELECT count(DISTINCT user_id) FROM match_players p
             JOIN matches m ON m.match_id = p.match_id
             WHERE m.channel_id = $1",
        )
        .bind(channel.get())
        .fetch_one(self.pool())
        .await?;
        Ok(ChannelTotals {
            completed: row.get("completed"),
            cancelled: row.get("cancelled"),
            live: row.get("live"),
            last_week: row.get("last_week"),
            distinct_players: players,
        })
    }
}

/// Aggregate match counts for one channel.
#[derive(Debug, Clone, Copy)]
pub struct ChannelTotals {
    /// Matches that finished with a result.
    pub completed: i64,
    /// Matches that were cancelled.
    pub cancelled: i64,
    /// Matches currently in progress.
    pub live: i64,
    /// Matches completed in the last seven days.
    pub last_week: i64,
    /// How many distinct players have ever appeared on a roster here.
    pub distinct_players: i64,
}
