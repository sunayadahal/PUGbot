//! The match lifecycle: launch, check-in, team formation, map selection,
//! substitution, reporting and finalisation.
//!
//! Every state change goes through [`MatchService::advance`], which drives the
//! match as far forward as the current facts allow and stops. Commands change a
//! fact (a ready press, a pick, a vote) and then call `advance`; the timer job
//! calls it too. That means there is exactly one place where transitions
//! happen, and a restart can resume simply by calling it again.

use chrono::{Duration, Utc};
use rand::thread_rng;

use super::{Announcement, AppContext};
use crate::domain::checkin::{CheckInOutcome, ReadyState};
use crate::domain::draft::PickOrder;
use crate::domain::ids::{ChannelId, MatchId, UserId};
use crate::domain::maps::select_maps;
use crate::domain::match_state::MatchState;
use crate::domain::queue::expiry_for;
use crate::domain::report::{Consensus, ReportOutcome};
use crate::domain::settings::{CaptainMode, QueueSettings, TeamFormationMode};
use crate::domain::teams::{balanced_teams, random_teams, select_captains, PlayerSeed};
use crate::error::{DomainError, ServiceError, ServiceResult};
use crate::repositories::matches::{LoadedMatch, NewMatch};
use crate::repositories::{ChannelConfigRow, QueueRow};
use crate::services::rating_svc::RatingService;

/// How many past matches to inspect when applying the map cooldown.
const RECENT_MAP_LOOKBACK: i64 = 20;

/// Drives the match lifecycle. See the [module documentation](self).
#[derive(Debug, Clone)]
pub struct MatchService {
    ctx: AppContext,
}

/// What a report attempt resulted in, so the caller can phrase the reply.
/// What a report attempt resulted in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReportStatus {
    /// Recorded, still waiting for the other side.
    Pending,
    /// Teams disagree; a moderator must settle it.
    Disputed,
    /// The match is finalised.
    Final(ReportOutcome),
}

impl MatchService {
    /// Wraps the shared application context.
    #[must_use]
    pub fn new(ctx: AppContext) -> Self {
        Self { ctx }
    }

    /// Creates a match from a roster and drives it to its first waiting state.
    ///
    /// The roster is removed from the queue in the same transaction as the
    /// match insert, so a crash in between cannot leave players in both.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Rejected`] for a roster of fewer than two,
    /// [`ServiceError::Domain`] if the queue settings fail validation or a
    /// player is already in a live match, or [`ServiceError::Database`].
    pub async fn launch(
        &self,
        channel: &ChannelConfigRow,
        queue: &QueueRow,
        roster: Vec<UserId>,
    ) -> ServiceResult<MatchId> {
        let settings = queue.settings.clone();
        settings.validate()?;
        if roster.len() < 2 {
            return Err(ServiceError::Rejected(
                "a match needs at least two players".to_string(),
            ));
        }

        let now = self.ctx.now();
        let has_check_in = settings.check_in.is_some();
        let state = if has_check_in {
            MatchState::CheckIn
        } else {
            MatchState::Queued
        };
        let check_in_ends_at = settings
            .check_in
            .as_ref()
            .map(|c| now + Duration::seconds(c.timeout_seconds));

        let new = NewMatch {
            guild: queue.guild,
            channel: queue.channel,
            queue: Some(queue.id),
            state,
            ranked: settings.ranked,
            rating_pool: channel.rating_pool(),
            settings,
            players: roster.clone(),
            mode: self.ctx.mode().to_string(),
            check_in_ends_at,
            expires_at: Some(now + Duration::seconds(queue.settings.match_lifetime_seconds)),
        };

        let mut tx = self.ctx.store.begin().await?;
        let id = self.ctx.store.create_match(&mut tx, &new).await?;
        for user in &roster {
            sqlx::query("DELETE FROM queue_members WHERE queue_id = $1 AND user_id = $2")
                .bind(queue.id.get())
                .bind(user.get())
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;

        // Auto-ready is a one-use preference: consume it as it is applied.
        if has_check_in {
            let mut armed = Vec::new();
            for user in &roster {
                let prefs = self.ctx.store.user_prefs(*user).await?;
                if crate::domain::queue::auto_ready_active(prefs.auto_ready_until, now) {
                    armed.push(*user);
                }
            }
            for user in &armed {
                self.ctx
                    .store
                    .set_ready_state(id, *user, ReadyState::Ready)
                    .await?;
            }
            self.ctx.store.consume_auto_ready(&armed).await?;
        }

        self.ctx
            .audit(
                Some(queue.guild),
                Some(queue.channel),
                None,
                "match.launched",
                Some(&id.to_string()),
                serde_json::json!({ "players": roster.len(), "ranked": new.ranked }),
            )
            .await;

        self.advance(id).await?;
        Ok(id)
    }

    /// Drives a match forward through every transition its current facts
    /// justify, and stops when it needs something from a player or a timer.
    ///
    /// Safe to call repeatedly: each step re-reads the match and re-checks its
    /// preconditions, so a double-clicked button or a retried job is a no-op.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::NoMatch`] if the match has been deleted,
    /// [`ServiceError::Conflict`] if another handler transitioned it first, or
    /// [`ServiceError::Database`].
    pub async fn advance(&self, id: MatchId) -> ServiceResult<()> {
        // Bounded so a logic error cannot spin: the state machine has nine
        // states, so more steps than that means something is looping.
        for _ in 0..MatchState::ALL.len() {
            let loaded = self.ctx.store.require_match(id).await?;
            let moved = match loaded.info.state {
                MatchState::CheckIn => self.step_check_in(&loaded).await?,
                MatchState::Queued | MatchState::TeamFormation => {
                    self.step_team_formation(&loaded).await?
                }
                MatchState::MapVote => self.step_map_vote(&loaded).await?,
                MatchState::Active | MatchState::ReportPending => false,
                state if state.is_terminal() => false,
                _ => false,
            };
            if !moved {
                return Ok(());
            }
        }
        Ok(())
    }

    async fn step_check_in(&self, loaded: &LoadedMatch) -> ServiceResult<bool> {
        let Some(settings) = loaded.info.settings.check_in.clone() else {
            // Configuration changed under a live match; fall through rather
            // than stranding it in CHECK_IN forever.
            return self.enter_team_formation(loaded).await.map(|()| true);
        };
        let Some(check_in) = loaded.check_in() else {
            return Ok(false);
        };

        match check_in.evaluate(&settings, self.ctx.now()) {
            CheckInOutcome::Waiting => Ok(false),
            CheckInOutcome::Passed => self.enter_team_formation(loaded).await.map(|()| true),
            CheckInOutcome::Failed { returned, dropped } => {
                self.fail_check_in(loaded, &returned, &dropped).await?;
                Ok(false)
            }
        }
    }

    /// Cancels the match and puts the players the policy keeps back into the
    /// queue, in one transaction.
    async fn fail_check_in(
        &self,
        loaded: &LoadedMatch,
        returned: &[UserId],
        dropped: &[UserId],
    ) -> ServiceResult<()> {
        let now = self.ctx.now();
        let channel = self
            .ctx
            .store
            .require_enabled_channel(loaded.info.channel)
            .await?;
        let queue = self
            .ctx
            .store
            .queue_for_channel(loaded.info.channel)
            .await?;

        let mut tx = self.ctx.store.begin().await?;
        self.ctx
            .store
            .transition(
                &mut tx,
                loaded.info.id,
                loaded.info.version,
                MatchState::Cancelled,
            )
            .await?;
        if let Some(queue) = &queue {
            for user in returned {
                let prefs = self.ctx.store.user_prefs(*user).await?;
                let expires_at =
                    expiry_for(now, None, prefs.default_expiry_seconds, &channel.settings);
                sqlx::query(
                    "INSERT INTO queue_members (queue_id, user_id, joined_at, expires_at)
                     VALUES ($1, $2, $3, $4) ON CONFLICT (queue_id, user_id) DO NOTHING",
                )
                .bind(queue.id.get())
                .bind(user.get())
                .bind(now)
                .bind(expires_at)
                .execute(&mut *tx)
                .await?;
            }
        }
        tx.commit().await?;

        self.ctx
            .audit(
                Some(loaded.info.guild),
                Some(loaded.info.channel),
                None,
                "match.check_in_failed",
                Some(&loaded.info.id.to_string()),
                serde_json::json!({ "returned": returned.len(), "dropped": dropped.len() }),
            )
            .await;

        let locale = self.ctx.locale(&channel.settings);
        self.ctx
            .notifier
            .announce(
                loaded.info.channel,
                Announcement::Text(locale.get("checkin.failed").to_string()),
            )
            .await;
        Ok(())
    }

    /// Moves into team formation, appointing captains or computing teams.
    async fn enter_team_formation(&self, loaded: &LoadedMatch) -> ServiceResult<()> {
        let settings = &loaded.info.settings;
        let seeds = self.seeds(loaded).await?;
        let team_count = settings.team_count as usize;
        let team_size = settings.team_size() as usize;

        let mut tx = self.ctx.store.begin().await?;
        let version = self
            .ctx
            .store
            .transition(
                &mut tx,
                loaded.info.id,
                loaded.info.version,
                MatchState::TeamFormation,
            )
            .await?;
        // The check-in deadline has served its purpose; clearing it keeps the
        // timer sweep from re-examining this match.
        self.ctx
            .store
            .set_check_in_deadline(&mut tx, loaded.info.id, None)
            .await?;

        match settings.team_formation {
            TeamFormationMode::NoTeams => {}
            TeamFormationMode::RandomTeams => {
                let teams = {
                    let mut rng = thread_rng();
                    random_teams(&seeds, team_count, team_size, &mut rng)?
                };
                self.ctx
                    .store
                    .set_teams(&mut tx, loaded.info.id, &teams, &vec![None; team_count])
                    .await?;
            }
            TeamFormationMode::RatingMatchmaking => {
                let teams = balanced_teams(&seeds, team_count, team_size)?;
                self.ctx
                    .store
                    .set_teams(&mut tx, loaded.info.id, &teams, &vec![None; team_count])
                    .await?;
            }
            TeamFormationMode::CaptainDraft => {
                let captains = {
                    let mut rng = thread_rng();
                    select_captains(&seeds, settings.captain_mode, team_count, &mut rng)?
                };
                // Volunteer mode appoints nobody; players claim slots later.
                let teams: Vec<Vec<UserId>> = captains.iter().map(|c| vec![*c]).collect();
                if !teams.is_empty() {
                    let slots: Vec<Option<UserId>> = captains.iter().copied().map(Some).collect();
                    self.ctx
                        .store
                        .set_teams(&mut tx, loaded.info.id, &teams, &slots)
                        .await?;
                }
            }
        }
        tx.commit().await?;

        // A draft has to wait for picks; anything else can move straight on.
        if settings.team_formation != TeamFormationMode::CaptainDraft {
            let refreshed = self.ctx.store.require_match(loaded.info.id).await?;
            self.enter_map_phase(&refreshed).await?;
        } else {
            let _ = version;
            self.ctx
                .notifier
                .announce(
                    loaded.info.channel,
                    Announcement::MatchUpdate(loaded.info.id),
                )
                .await;
        }
        Ok(())
    }

    async fn step_team_formation(&self, loaded: &LoadedMatch) -> ServiceResult<bool> {
        let settings = &loaded.info.settings;
        if loaded.info.state == MatchState::Queued {
            self.enter_team_formation(loaded).await?;
            return Ok(true);
        }
        if settings.team_formation != TeamFormationMode::CaptainDraft {
            return Ok(false);
        }
        let draft = loaded.draft()?;
        if draft.is_complete() {
            self.enter_map_phase(loaded).await?;
            return Ok(true);
        }
        Ok(false)
    }

    /// Opens a map vote, or picks maps outright, then activates the match.
    async fn enter_map_phase(&self, loaded: &LoadedMatch) -> ServiceResult<()> {
        let settings = &loaded.info.settings;
        let now = self.ctx.now();

        if settings.maps.pool.is_empty() {
            return self.activate(loaded).await;
        }

        let recent = self
            .ctx
            .store
            .recent_maps(loaded.info.channel, RECENT_MAP_LOOKBACK)
            .await?;

        if let Some(vote) = settings.maps.vote.clone() {
            let candidates = {
                let mut rng = thread_rng();
                select_maps(
                    &settings.maps.pool,
                    vote.candidates as usize,
                    &recent,
                    settings.maps.cooldown_matches as usize,
                    &mut rng,
                )?
            };
            let mut tx = self.ctx.store.begin().await?;
            self.ctx
                .store
                .transition(
                    &mut tx,
                    loaded.info.id,
                    loaded.info.version,
                    MatchState::MapVote,
                )
                .await?;
            self.ctx
                .store
                .set_map_candidates(&mut tx, loaded.info.id, &candidates)
                .await?;
            // The vote shares the check-in timeout when one is configured, and
            // otherwise gets a short fixed window.
            let window = settings
                .check_in
                .as_ref()
                .map_or(60, |c| c.timeout_seconds.min(120));
            self.ctx
                .store
                .set_vote_deadline(
                    &mut tx,
                    loaded.info.id,
                    Some(now + Duration::seconds(window)),
                )
                .await?;
            tx.commit().await?;
            self.ctx
                .notifier
                .announce(
                    loaded.info.channel,
                    Announcement::MatchUpdate(loaded.info.id),
                )
                .await;
            return Ok(());
        }

        let maps = {
            let mut rng = thread_rng();
            select_maps(
                &settings.maps.pool,
                settings.maps.pick_count as usize,
                &recent,
                settings.maps.cooldown_matches as usize,
                &mut rng,
            )?
        };
        let mut tx = self.ctx.store.begin().await?;
        self.ctx
            .store
            .set_maps(&mut tx, loaded.info.id, &maps)
            .await?;
        tx.commit().await?;

        let refreshed = self.ctx.store.require_match(loaded.info.id).await?;
        self.activate(&refreshed).await
    }

    async fn step_map_vote(&self, loaded: &LoadedMatch) -> ServiceResult<bool> {
        let Some(vote) = loaded.map_vote() else {
            return Ok(false);
        };
        let deadline_passed = loaded
            .info
            .vote_ends_at
            .is_some_and(|at| at <= self.ctx.now());
        if !vote.everyone_voted() && !deadline_passed {
            return Ok(false);
        }

        let settings = &loaded.info.settings;
        let tie_break = settings
            .maps
            .vote
            .as_ref()
            .map_or(crate::domain::settings::TieBreak::Random, |v| v.tie_break);
        let maps = {
            let mut rng = thread_rng();
            vote.resolve(
                settings.maps.pick_count.max(1) as usize,
                tie_break,
                &mut rng,
            )
        };

        let mut tx = self.ctx.store.begin().await?;
        self.ctx
            .store
            .set_maps(&mut tx, loaded.info.id, &maps)
            .await?;
        self.ctx
            .store
            .set_vote_deadline(&mut tx, loaded.info.id, None)
            .await?;
        tx.commit().await?;

        let refreshed = self.ctx.store.require_match(loaded.info.id).await?;
        self.activate(&refreshed).await?;
        Ok(true)
    }

    /// Marks the match active, snapshots rating inputs, and sends start DMs.
    async fn activate(&self, loaded: &LoadedMatch) -> ServiceResult<()> {
        let mut tx = self.ctx.store.begin().await?;
        self.ctx
            .store
            .transition(
                &mut tx,
                loaded.info.id,
                loaded.info.version,
                MatchState::Active,
            )
            .await?;

        // Snapshot each player's rating so the result can be explained later
        // even if their rating moves in another match first.
        if loaded.info.ranked {
            let channel = self
                .ctx
                .store
                .require_enabled_channel(loaded.info.channel)
                .await?;
            let stats = self
                .ctx
                .store
                .player_stats_bulk(
                    loaded.info.rating_pool,
                    &loaded.roster(),
                    &channel.settings.rating,
                )
                .await?;
            for player in stats {
                sqlx::query(
                    "UPDATE match_players SET rating_before = $3, deviation_before = $4
                     WHERE match_id = $1 AND user_id = $2",
                )
                .bind(loaded.info.id.get())
                .bind(player.user.get())
                .bind(player.rating)
                .bind(player.deviation)
                .execute(&mut *tx)
                .await?;
            }
        }
        tx.commit().await?;

        self.ctx
            .notifier
            .announce(
                loaded.info.channel,
                Announcement::MatchUpdate(loaded.info.id),
            )
            .await;

        if loaded.info.settings.start_dm {
            self.send_start_dms(loaded).await;
        }
        Ok(())
    }

    /// Sends start DMs, honouring each player's preference.
    ///
    /// A player with DMs closed is skipped without failing the match start;
    /// Discord failures are the notifier's problem, not the match's.
    async fn send_start_dms(&self, loaded: &LoadedMatch) {
        let text = format!(
            "Your match #{} in <#{}> has started.",
            loaded.info.id, loaded.info.channel
        );
        for user in loaded.roster() {
            match self.ctx.store.user_prefs(user).await {
                Ok(prefs) if prefs.dm_on_start => {
                    self.ctx.notifier.direct_message(user, text.clone()).await;
                }
                Ok(_) => {}
                Err(error) => tracing::warn!(%user, %error, "could not read DM preference"),
            }
        }
    }

    /// Rating and captain-role inputs for team formation.
    async fn seeds(&self, loaded: &LoadedMatch) -> ServiceResult<Vec<PlayerSeed>> {
        let channel = self
            .ctx
            .store
            .require_enabled_channel(loaded.info.channel)
            .await?;
        let stats = self
            .ctx
            .store
            .player_stats_bulk(
                loaded.info.rating_pool,
                &loaded.roster(),
                &channel.settings.rating,
            )
            .await?;
        Ok(stats
            .into_iter()
            .map(|row| PlayerSeed {
                user: row.user,
                rating: row.rating,
                // Captain-role membership is a Discord fact the adapter
                // supplies when it matters; formation still works without it.
                has_captain_role: false,
            })
            .collect())
    }

    // ------------------------------------------------------------- commands

    /// Records a ready-check answer and advances the match if that settles it.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::UnexpectedState`] if the match is not in
    /// check-in, [`DomainError::NotInMatch`] if the caller is not on the
    /// roster, or [`ServiceError::Database`].
    pub async fn set_ready(&self, id: MatchId, user: UserId, ready: bool) -> ServiceResult<()> {
        let loaded = self.ctx.store.require_match(id).await?;
        loaded.info.state.ensure_is(MatchState::CheckIn)?;
        if !loaded.contains(user) {
            return Err(DomainError::NotInMatch.into());
        }
        let state = if ready {
            ReadyState::Ready
        } else {
            ReadyState::Declined
        };
        self.ctx.store.set_ready_state(id, user, state).await?;
        self.ctx
            .notifier
            .announce(loaded.info.channel, Announcement::MatchUpdate(id))
            .await;
        self.advance(id).await
    }

    /// `/pick`: the captain on the clock chooses a player.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::UnexpectedState`] if the match is not drafting,
    /// [`DomainError::NotActiveCaptain`] if it is not this captain's turn,
    /// [`DomainError::PlayerNotInPool`] if the target is unavailable, or
    /// [`ServiceError::Database`].
    pub async fn pick(&self, id: MatchId, captain: UserId, player: UserId) -> ServiceResult<()> {
        let loaded = self.ctx.store.require_match(id).await?;
        loaded.info.state.ensure_is(MatchState::TeamFormation)?;
        let mut draft = loaded.draft()?;
        let outcome = draft.pick(captain, player)?;

        let mut tx = self.ctx.store.begin().await?;
        self.ctx
            .store
            .set_teams(&mut tx, id, &draft.teams, &draft.captains)
            .await?;
        self.ctx
            .store
            .record_picks(&mut tx, id, &outcome.picks)
            .await?;
        tx.commit().await?;

        self.ctx
            .notifier
            .announce(loaded.info.channel, Announcement::MatchUpdate(id))
            .await;
        self.advance(id).await
    }

    /// `/capfor`: claim an empty captain slot.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Domain`] carrying
    /// [`DomainError::UnexpectedState`] if the match is not drafting,
    /// [`DomainError::NoSuchTeam`], [`DomainError::CaptainSlotTaken`], or
    /// [`DomainError::PlayerNotInPool`]; otherwise
    /// [`ServiceError::Database`].
    pub async fn claim_captain(&self, id: MatchId, user: UserId, team: usize) -> ServiceResult<()> {
        let loaded = self.ctx.store.require_match(id).await?;
        loaded.info.state.ensure_is(MatchState::TeamFormation)?;
        let mut draft = loaded.draft()?;
        draft.set_captain(team, user)?;

        let mut tx = self.ctx.store.begin().await?;
        self.ctx
            .store
            .set_teams(&mut tx, id, &draft.teams, &draft.captains)
            .await?;
        tx.commit().await?;
        self.ctx
            .notifier
            .announce(loaded.info.channel, Announcement::MatchUpdate(id))
            .await;
        self.advance(id).await
    }

    /// `/capme`: step down from a captain slot, returning the team vacated.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Domain`] carrying
    /// [`DomainError::NotActiveCaptain`] if the caller captains nothing, or
    /// [`DomainError::InvalidConfig`] if they have already picked; otherwise
    /// [`ServiceError::Database`].
    pub async fn vacate_captain(&self, id: MatchId, user: UserId) -> ServiceResult<usize> {
        let loaded = self.ctx.store.require_match(id).await?;
        loaded.info.state.ensure_is(MatchState::TeamFormation)?;
        let mut draft = loaded.draft()?;
        let team = draft.vacate_captain(user)?;

        let mut tx = self.ctx.store.begin().await?;
        self.ctx
            .store
            .set_teams(&mut tx, id, &draft.teams, &draft.captains)
            .await?;
        tx.commit().await?;
        self.ctx
            .notifier
            .announce(loaded.info.channel, Announcement::MatchUpdate(id))
            .await;
        Ok(team)
    }

    /// Records a map ballot and closes the vote if everybody has voted.
    ///
    /// The ballot is validated by the domain first, so an out-of-range
    /// candidate or a vote from an outsider never reaches the database.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::UnexpectedState`] if no vote is open,
    /// [`DomainError::NotInMatch`] if the caller may not vote, or
    /// [`ServiceError::Database`].
    pub async fn cast_vote(
        &self,
        id: MatchId,
        user: UserId,
        candidate: usize,
    ) -> ServiceResult<()> {
        let loaded = self.ctx.store.require_match(id).await?;
        loaded.info.state.ensure_is(MatchState::MapVote)?;
        let mut vote = loaded.map_vote().ok_or(ServiceError::NoMatch)?;
        // Validate through the domain first so an out-of-range candidate or an
        // outsider never reaches the database.
        vote.cast(user, candidate)?;
        self.ctx.store.cast_map_vote(id, user, candidate).await?;
        self.advance(id).await
    }

    /// A player reports a result. Finalises when every team agrees.
    ///
    /// A queue with no teams has nobody to disagree with, so the first report
    /// from a participant is final.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Domain`] carrying
    /// [`DomainError::UnexpectedState`] if the match is not active or pending,
    /// or [`DomainError::NotInMatch`] if the reporter is not on the roster;
    /// otherwise [`ServiceError::Database`].
    pub async fn report(
        &self,
        id: MatchId,
        user: UserId,
        outcome: ReportOutcome,
    ) -> ServiceResult<ReportStatus> {
        let loaded = self.ctx.store.require_match(id).await?;
        if !matches!(
            loaded.info.state,
            MatchState::Active | MatchState::ReportPending
        ) {
            return Err(DomainError::UnexpectedState {
                expected: MatchState::Active,
                actual: loaded.info.state,
            }
            .into());
        }
        if !loaded.contains(user) {
            return Err(DomainError::NotInMatch.into());
        }

        self.ctx.store.record_report(id, user, outcome).await?;
        let refreshed = self.ctx.store.require_match(id).await?;
        let rosters = refreshed.rosters();

        // With no teams there is nobody to disagree with, so the first report
        // from a participant is final.
        if !refreshed.info.settings.uses_teams() {
            self.finalize(&refreshed, outcome, None, None).await?;
            return Ok(ReportStatus::Final(outcome));
        }

        match refreshed.report_ledger().evaluate(&rosters)? {
            Consensus::Agreed(agreed) => {
                self.finalize(&refreshed, agreed, None, None).await?;
                Ok(ReportStatus::Final(agreed))
            }
            Consensus::Disputed => {
                self.mark_report_pending(&refreshed).await?;
                Ok(ReportStatus::Disputed)
            }
            Consensus::Pending { .. } => {
                self.mark_report_pending(&refreshed).await?;
                Ok(ReportStatus::Pending)
            }
        }
    }

    async fn mark_report_pending(&self, loaded: &LoadedMatch) -> ServiceResult<()> {
        if loaded.info.state == MatchState::ReportPending {
            return Ok(());
        }
        let mut tx = self.ctx.store.begin().await?;
        self.ctx
            .store
            .transition(
                &mut tx,
                loaded.info.id,
                loaded.info.version,
                MatchState::ReportPending,
            )
            .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Moderator override: force a result regardless of consensus.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Domain`] carrying
    /// [`DomainError::AlreadyFinalized`] if the match has already ended, or
    /// [`ServiceError::Database`].
    pub async fn moderator_report(
        &self,
        id: MatchId,
        actor: UserId,
        outcome: ReportOutcome,
        scores: Option<Vec<i32>>,
    ) -> ServiceResult<()> {
        let loaded = self.ctx.store.require_match(id).await?;
        if loaded.info.state.is_terminal() {
            return Err(DomainError::AlreadyFinalized.into());
        }
        self.finalize(&loaded, outcome, scores, Some(actor)).await
    }

    /// Writes the result, ends the match, and applies ratings exactly once.
    async fn finalize(
        &self,
        loaded: &LoadedMatch,
        outcome: ReportOutcome,
        scores: Option<Vec<i32>>,
        actor: Option<UserId>,
    ) -> ServiceResult<()> {
        let target = if outcome == ReportOutcome::Cancel {
            MatchState::Cancelled
        } else {
            MatchState::Completed
        };

        let mut tx = self.ctx.store.begin().await?;
        self.ctx
            .store
            .set_outcome(&mut tx, loaded.info.id, outcome, scores.as_deref())
            .await?;
        self.ctx
            .store
            .transition(&mut tx, loaded.info.id, loaded.info.version, target)
            .await?;

        // `claim_rating` flips a one-shot flag inside this transaction. If a
        // concurrent finalisation already claimed it, this one rates nothing,
        // and the unique index on rating_history is the second line of defence.
        let should_rate = loaded.info.ranked
            && outcome.is_rated()
            && self.ctx.store.claim_rating(&mut tx, loaded.info.id).await?;

        if should_rate {
            let channel = self
                .ctx
                .store
                .require_enabled_channel(loaded.info.channel)
                .await?;
            RatingService::new(self.ctx.clone())
                .apply_match_result(&mut tx, loaded, outcome, &channel)
                .await?;
        }
        tx.commit().await?;

        self.ctx
            .audit(
                Some(loaded.info.guild),
                Some(loaded.info.channel),
                actor,
                if actor.is_some() {
                    "match.moderator_report"
                } else {
                    "match.reported"
                },
                Some(&loaded.info.id.to_string()),
                serde_json::json!({ "outcome": outcome, "rated": should_rate }),
            )
            .await;

        self.ctx
            .notifier
            .announce(
                loaded.info.channel,
                Announcement::MatchFinished(loaded.info.id, format!("{outcome:?}")),
            )
            .await;
        Ok(())
    }

    /// Cancels a match. Players are released and nothing is rated.
    ///
    /// # Errors
    ///
    /// Returns [`DomainError::AlreadyFinalized`] if the match has already
    /// ended, or [`ServiceError::Database`].
    pub async fn cancel(&self, id: MatchId, actor: Option<UserId>) -> ServiceResult<()> {
        let loaded = self.ctx.store.require_match(id).await?;
        if loaded.info.state.is_terminal() {
            return Err(DomainError::AlreadyFinalized.into());
        }
        let mut tx = self.ctx.store.begin().await?;
        self.ctx
            .store
            .transition(&mut tx, id, loaded.info.version, MatchState::Cancelled)
            .await?;
        tx.commit().await?;
        self.ctx
            .audit(
                Some(loaded.info.guild),
                Some(loaded.info.channel),
                actor,
                "match.cancelled",
                Some(&id.to_string()),
                serde_json::json!({}),
            )
            .await;
        self.ctx
            .notifier
            .announce(
                loaded.info.channel,
                Announcement::MatchFinished(id, "cancelled".to_string()),
            )
            .await;
        Ok(())
    }

    /// Replaces a player. Works in every live state, including mid-draft.
    ///
    /// The substitute inherits the outgoing player's team and captaincy, and is
    /// removed from the channel's queue if they were sitting in it.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Domain`] carrying
    /// [`DomainError::AlreadyFinalized`], [`DomainError::NotInMatch`] if `out`
    /// is not on the roster, or [`DomainError::AlreadyInMatch`] if `into`
    /// already is; otherwise [`ServiceError::Database`].
    pub async fn substitute(&self, id: MatchId, out: UserId, into: UserId) -> ServiceResult<()> {
        let loaded = self.ctx.store.require_match(id).await?;
        if loaded.info.state.is_terminal() {
            return Err(DomainError::AlreadyFinalized.into());
        }
        if !loaded.contains(out) {
            return Err(DomainError::NotInMatch.into());
        }
        if loaded.contains(into) {
            return Err(DomainError::AlreadyInMatch.into());
        }

        let mut tx = self.ctx.store.begin().await?;
        self.ctx
            .store
            .substitute_player(&mut tx, id, loaded.info.channel, out, into)
            .await?;
        // A substitute joining during a queue also leaves that queue.
        if let Some(queue) = self
            .ctx
            .store
            .queue_for_channel(loaded.info.channel)
            .await?
        {
            sqlx::query("DELETE FROM queue_members WHERE queue_id = $1 AND user_id = $2")
                .bind(queue.id.get())
                .bind(into.get())
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;

        self.ctx
            .audit(
                Some(loaded.info.guild),
                Some(loaded.info.channel),
                None,
                "match.substituted",
                Some(&id.to_string()),
                serde_json::json!({ "out": out.get(), "into": into.get() }),
            )
            .await;
        self.ctx
            .notifier
            .announce(loaded.info.channel, Announcement::MatchUpdate(id))
            .await;
        Ok(())
    }

    /// Moderator override: move a player onto a team or back into the pool.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Domain`] carrying
    /// [`DomainError::AlreadyFinalized`], [`DomainError::NoSuchTeam`], or
    /// [`DomainError::NotInMatch`]; otherwise [`ServiceError::Database`].
    pub async fn place_player(
        &self,
        id: MatchId,
        user: UserId,
        team: Option<usize>,
    ) -> ServiceResult<()> {
        let loaded = self.ctx.store.require_match(id).await?;
        if loaded.info.state.is_terminal() {
            return Err(DomainError::AlreadyFinalized.into());
        }
        if let Some(team) = team {
            if team >= loaded.info.settings.team_count as usize {
                return Err(DomainError::NoSuchTeam(team).into());
            }
        }
        if !loaded.contains(user) {
            return Err(DomainError::NotInMatch.into());
        }
        let mut tx = self.ctx.store.begin().await?;
        self.ctx.store.place_player(&mut tx, id, user, team).await?;
        tx.commit().await?;
        self.ctx
            .notifier
            .announce(loaded.info.channel, Announcement::MatchUpdate(id))
            .await;
        self.advance(id).await
    }

    /// Creates a finished ranked match from a moderator-supplied roster, for
    /// results played outside the bot.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Rejected`] if a player appears on more than one
    /// team, [`ServiceError::Domain`] if a player is already in a live match,
    /// or [`ServiceError::Database`].
    pub async fn create_historical(
        &self,
        channel: &ChannelConfigRow,
        settings: QueueSettings,
        teams: Vec<Vec<UserId>>,
        outcome: ReportOutcome,
        actor: UserId,
    ) -> ServiceResult<MatchId> {
        let roster: Vec<UserId> = teams.concat();
        let mut unique = roster.clone();
        unique.sort_unstable();
        unique.dedup();
        if unique.len() != roster.len() {
            return Err(ServiceError::Rejected(
                "a player is listed on more than one team".to_string(),
            ));
        }

        let new = NewMatch {
            guild: channel.guild,
            channel: channel.channel,
            queue: None,
            state: MatchState::Active,
            ranked: settings.ranked,
            rating_pool: channel.rating_pool(),
            settings,
            players: roster,
            mode: self.ctx.mode().to_string(),
            check_in_ends_at: None,
            expires_at: None,
        };

        let mut tx = self.ctx.store.begin().await?;
        let id = self.ctx.store.create_match(&mut tx, &new).await?;
        let captains = vec![None; teams.len()];
        self.ctx
            .store
            .set_teams(&mut tx, id, &teams, &captains)
            .await?;
        tx.commit().await?;

        let loaded = self.ctx.store.require_match(id).await?;
        self.finalize(&loaded, outcome, None, Some(actor)).await?;
        Ok(id)
    }

    /// Expires matches past their lifetime and pushes timed-out check-ins and
    /// votes along. Returns how many matches were handled.
    ///
    /// Called by the timer job and at startup. A conflict on any one match is
    /// swallowed — another worker got there first, which is exactly what the
    /// optimistic lock is for — and any other per-match error is logged so one
    /// bad match cannot stop the sweep.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`] if the work list itself cannot be
    /// read.
    pub async fn process_due(&self) -> ServiceResult<usize> {
        let now = self.ctx.now();
        let due = self.ctx.store.matches_past_deadline(now).await?;
        let mut handled = 0;
        for loaded in due {
            let expired = loaded.info.expires_at.is_some_and(|at| at <= now);
            let result = if expired && loaded.info.state != MatchState::MapVote {
                self.expire(&loaded).await
            } else {
                self.advance(loaded.info.id).await
            };
            match result {
                Ok(()) => handled += 1,
                // A conflict means another worker got there first, which is
                // exactly what the optimistic lock is for.
                Err(ServiceError::Conflict(_)) => {}
                Err(error) => {
                    tracing::error!(match_id = %loaded.info.id, %error, "failed to process due match");
                }
            }
        }
        Ok(handled)
    }

    /// A ranked match that outlives its configured lifetime ends unrated: the
    /// bot has no evidence of what happened, so inventing a result would
    /// corrupt ratings.
    async fn expire(&self, loaded: &LoadedMatch) -> ServiceResult<()> {
        let mut tx = self.ctx.store.begin().await?;
        self.ctx
            .store
            .transition(
                &mut tx,
                loaded.info.id,
                loaded.info.version,
                MatchState::Expired,
            )
            .await?;
        tx.commit().await?;
        self.ctx
            .audit(
                Some(loaded.info.guild),
                Some(loaded.info.channel),
                None,
                "match.expired",
                Some(&loaded.info.id.to_string()),
                serde_json::json!({ "state": loaded.info.state.as_str() }),
            )
            .await;
        self.ctx
            .notifier
            .announce(
                loaded.info.channel,
                Announcement::MatchFinished(loaded.info.id, "expired".to_string()),
            )
            .await;
        Ok(())
    }

    /// The live match a player is in, within the channel's configured scope.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceError::Database`] if the query fails.
    pub async fn live_match_for(
        &self,
        user: UserId,
        channel: ChannelId,
        settings: &crate::domain::settings::ChannelSettings,
        guild: crate::domain::ids::GuildId,
    ) -> ServiceResult<Option<LoadedMatch>> {
        let scope = match settings.queue_scope {
            crate::domain::settings::QueueScope::Guild => Some(guild),
            crate::domain::settings::QueueScope::Channel => None,
        };
        self.ctx
            .store
            .live_match_for_user(user, channel, scope)
            .await
    }
}

/// Default settings for a moderator-created historical ranked match.
///
/// Deliberately minimal: no check-in, no autostart, no start DMs, because the
/// match has already been played elsewhere.
pub fn historical_settings(team_count: usize, size: usize) -> QueueSettings {
    QueueSettings {
        name: "manual".to_string(),
        size: size as u32,
        team_count: team_count as u32,
        ranked: true,
        autostart: false,
        team_formation: TeamFormationMode::RandomTeams,
        captain_mode: CaptainMode::Random,
        pick_order: PickOrder::default(),
        check_in: None,
        start_dm: false,
        match_lifetime_seconds: 3600,
        ..QueueSettings::default()
    }
}

/// Seconds from `now` until `deadline`, floored at zero so a passed deadline
/// renders as `0` rather than a negative countdown.
#[must_use]
pub fn seconds_until(deadline: chrono::DateTime<Utc>, now: chrono::DateTime<Utc>) -> i64 {
    (deadline - now).num_seconds().max(0)
}
