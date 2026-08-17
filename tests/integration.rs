//! Database integration tests.
//!
//! These cover the parts of the system that only a real PostgreSQL instance can
//! prove: schema constraints, concurrency, transactional finalisation, and
//! restart recovery. See `tests/common/mod.rs` for how to point them at a
//! database.

mod common;

use chrono::Duration;
use pugbot::domain::ids::{ChannelId, MatchId, UserId};
use pugbot::domain::match_state::MatchState;
use pugbot::domain::rating::{RatingConfig, RatingDelta, RatingSystemKind};
use pugbot::domain::report::ReportOutcome;
use pugbot::domain::settings::{
    CheckInReturnPolicy, CheckInSettings, MapSettings, MapVoteSettings, QueueSettings,
    TeamFormationMode, TieBreak,
};
use pugbot::error::{DomainError, ServiceError};
use pugbot::repositories::matches::NewMatch;
use pugbot::services::match_svc::MatchService;
use pugbot::services::queue_svc::QueueService;

use common::TestApp;

fn ranked_queue(size: u32) -> QueueSettings {
    QueueSettings {
        size,
        ranked: true,
        autostart: false,
        team_formation: TeamFormationMode::RandomTeams,
        ..QueueSettings::default()
    }
}

#[tokio::test]
async fn migrations_apply_to_an_empty_database() {
    let url = require_database!();
    let app = TestApp::start(&url).await;
    // Re-running must be a no-op, which is what makes deploys safe.
    app.store
        .migrate()
        .await
        .expect("migrations are idempotent");
    app.store.ping().await.expect("database is reachable");
}

#[tokio::test]
async fn a_channel_can_only_ever_own_one_queue() {
    let url = require_database!();
    let app = TestApp::start(&url).await;
    app.with_queue(QueueSettings::default()).await;

    let second = app
        .store
        .create_queue(app.guild, app.channel, &QueueSettings::default())
        .await;
    assert!(
        matches!(second, Err(ServiceError::QueueExists)),
        "the unique index on channel_id must reject a second queue, got {second:?}"
    );
}

#[tokio::test]
async fn concurrent_joins_never_exceed_the_queue_size() {
    let url = require_database!();
    let app = TestApp::start(&url).await;
    let queue = app
        .with_queue(QueueSettings {
            size: 4,
            autostart: false,
            ..QueueSettings::default()
        })
        .await;

    let queues = QueueService::new(app.app.clone());
    let context = queues.context(app.channel).await.expect("queue context");

    // Ten players race for four slots.
    let mut handles = Vec::new();
    for id in 1..=10i64 {
        let queues = QueueService::new(app.app.clone());
        let context = context.clone();
        handles.push(tokio::spawn(async move {
            queues.add(&context, UserId(id), &[], None).await.is_ok()
        }));
    }
    let mut accepted = 0;
    for handle in handles {
        if handle.await.expect("join task") {
            accepted += 1;
        }
    }

    let members = app.store.queue_members(queue.id).await.expect("members");
    assert!(accepted <= 4, "{accepted} joins were accepted for 4 slots");
    assert_eq!(
        members.len(),
        accepted,
        "accepted joins must match stored rows"
    );
    assert!(members.len() <= 4);
}

#[tokio::test]
async fn the_same_player_cannot_join_twice() {
    let url = require_database!();
    let app = TestApp::start(&url).await;
    app.with_queue(QueueSettings {
        autostart: false,
        ..QueueSettings::default()
    })
    .await;

    let queues = QueueService::new(app.app.clone());
    let context = queues.context(app.channel).await.expect("queue context");
    queues
        .add(&context, UserId(1), &[], None)
        .await
        .expect("first join");
    let second = queues.add(&context, UserId(1), &[], None).await;
    assert!(matches!(
        second,
        Err(ServiceError::Domain(DomainError::AlreadyQueued))
    ));
}

#[tokio::test]
async fn a_player_cannot_be_in_two_live_matches_in_one_channel() {
    let url = require_database!();
    let app = TestApp::start(&url).await;
    let queue = app.with_queue(ranked_queue(2)).await;

    let new = |players: Vec<UserId>| NewMatch {
        guild: app.guild,
        channel: app.channel,
        queue: Some(queue.id),
        state: MatchState::Active,
        ranked: true,
        rating_pool: app.channel,
        settings: ranked_queue(2),
        players,
        mode: "debug".to_string(),
        check_in_ends_at: None,
        expires_at: None,
    };

    let mut tx = app.store.begin().await.expect("begin");
    app.store
        .create_match(&mut tx, &new(vec![UserId(1), UserId(2)]))
        .await
        .expect("first match");
    tx.commit().await.expect("commit");

    let mut tx = app.store.begin().await.expect("begin");
    let second = app
        .store
        .create_match(&mut tx, &new(vec![UserId(1), UserId(3)]))
        .await;
    assert!(
        matches!(
            second,
            Err(ServiceError::Domain(DomainError::AlreadyInMatch))
        ),
        "the partial unique index must reject a second live match, got {second:?}"
    );
}

#[tokio::test]
async fn a_finished_match_releases_its_players() {
    let url = require_database!();
    let app = TestApp::start(&url).await;
    let queue = app.with_queue(ranked_queue(2)).await;

    let new = |players: Vec<UserId>| NewMatch {
        guild: app.guild,
        channel: app.channel,
        queue: Some(queue.id),
        state: MatchState::Active,
        ranked: true,
        rating_pool: app.channel,
        settings: ranked_queue(2),
        players,
        mode: "debug".to_string(),
        check_in_ends_at: None,
        expires_at: None,
    };

    let mut tx = app.store.begin().await.expect("begin");
    let first = app
        .store
        .create_match(&mut tx, &new(vec![UserId(1), UserId(2)]))
        .await
        .expect("first match");
    tx.commit().await.expect("commit");

    MatchService::new(app.app.clone())
        .cancel(first, None)
        .await
        .expect("cancel");

    // The same players can now start a fresh match.
    let mut tx = app.store.begin().await.expect("begin");
    app.store
        .create_match(&mut tx, &new(vec![UserId(1), UserId(2)]))
        .await
        .expect("a cancelled match must free its players");
    tx.commit().await.expect("commit");
}

#[tokio::test]
async fn optimistic_locking_rejects_a_stale_transition() {
    let url = require_database!();
    let app = TestApp::start(&url).await;
    let queue = app.with_queue(ranked_queue(2)).await;

    let mut tx = app.store.begin().await.expect("begin");
    let id = app
        .store
        .create_match(
            &mut tx,
            &NewMatch {
                guild: app.guild,
                channel: app.channel,
                queue: Some(queue.id),
                state: MatchState::Active,
                ranked: true,
                rating_pool: app.channel,
                settings: ranked_queue(2),
                players: vec![UserId(1), UserId(2)],
                mode: "debug".to_string(),
                check_in_ends_at: None,
                expires_at: None,
            },
        )
        .await
        .expect("create");
    tx.commit().await.expect("commit");

    let loaded = app.store.require_match(id).await.expect("load");
    let stale_version = loaded.info.version;

    let mut tx = app.store.begin().await.expect("begin");
    app.store
        .transition(&mut tx, id, stale_version, MatchState::ReportPending)
        .await
        .expect("first transition wins");
    tx.commit().await.expect("commit");

    let mut tx = app.store.begin().await.expect("begin");
    let second = app
        .store
        .transition(&mut tx, id, stale_version, MatchState::Completed)
        .await;
    assert!(
        matches!(second, Err(ServiceError::Conflict(_))),
        "a stale version must be rejected, got {second:?}"
    );
}

#[tokio::test]
async fn a_ranked_match_is_rated_exactly_once() {
    let url = require_database!();
    let app = TestApp::start(&url).await;
    let queue = app.with_queue(ranked_queue(2)).await;

    let mut tx = app.store.begin().await.expect("begin");
    let id = app
        .store
        .create_match(
            &mut tx,
            &NewMatch {
                guild: app.guild,
                channel: app.channel,
                queue: Some(queue.id),
                state: MatchState::Active,
                ranked: true,
                rating_pool: app.channel,
                settings: ranked_queue(2),
                players: vec![UserId(1), UserId(2)],
                mode: "debug".to_string(),
                check_in_ends_at: None,
                expires_at: None,
            },
        )
        .await
        .expect("create");
    app.store
        .set_teams(
            &mut tx,
            id,
            &[vec![UserId(1)], vec![UserId(2)]],
            &[None, None],
        )
        .await
        .expect("teams");
    tx.commit().await.expect("commit");

    let matches = MatchService::new(app.app.clone());
    // Both players agree, which finalises the match.
    matches
        .report(id, UserId(1), ReportOutcome::Win(0))
        .await
        .expect("first report");
    matches
        .report(id, UserId(2), ReportOutcome::Win(0))
        .await
        .expect("second report");

    let history: i64 =
        sqlx::query_scalar("SELECT count(*) FROM rating_history WHERE match_id = $1")
            .bind(id.get())
            .fetch_one(app.store.pool())
            .await
            .expect("count history");
    assert_eq!(history, 2, "one rating row per player");

    // A repeated report on a finished match must change nothing.
    let repeat = matches.report(id, UserId(2), ReportOutcome::Win(0)).await;
    assert!(
        repeat.is_err(),
        "a completed match cannot be reported again"
    );

    let history_after: i64 =
        sqlx::query_scalar("SELECT count(*) FROM rating_history WHERE match_id = $1")
            .bind(id.get())
            .fetch_one(app.store.pool())
            .await
            .expect("count history");
    assert_eq!(history_after, 2, "ratings must not be applied twice");
}

#[tokio::test]
async fn the_rating_history_index_blocks_a_duplicate_write() {
    let url = require_database!();
    let app = TestApp::start(&url).await;
    let queue = app.with_queue(ranked_queue(2)).await;

    let mut tx = app.store.begin().await.expect("begin");
    let id = app
        .store
        .create_match(
            &mut tx,
            &NewMatch {
                guild: app.guild,
                channel: app.channel,
                queue: Some(queue.id),
                state: MatchState::Active,
                ranked: true,
                rating_pool: app.channel,
                settings: ranked_queue(2),
                players: vec![UserId(1), UserId(2)],
                mode: "debug".to_string(),
                check_in_ends_at: None,
                expires_at: None,
            },
        )
        .await
        .expect("create");
    tx.commit().await.expect("commit");

    let delta = RatingDelta {
        user: UserId(1),
        rating_before: 1500.0,
        rating_after: 1525.0,
        deviation_before: 200.0,
        deviation_after: 190.0,
        volatility_after: 0.06,
    };

    let mut tx = app.store.begin().await.expect("begin");
    let first = app
        .store
        .record_rating_change(&mut tx, app.channel, Some(id), &delta, "match", None)
        .await
        .expect("first write");
    let second = app
        .store
        .record_rating_change(&mut tx, app.channel, Some(id), &delta, "match", None)
        .await
        .expect("second write is a no-op, not an error");
    tx.commit().await.expect("commit");

    assert!(first, "the first write must insert");
    assert!(!second, "the second write must be swallowed by the index");

    // Manual adjustments have no match id, so they are never deduplicated.
    let mut tx = app.store.begin().await.expect("begin");
    assert!(app
        .store
        .record_rating_change(
            &mut tx,
            app.channel,
            None,
            &delta,
            "penalty",
            Some(UserId(9))
        )
        .await
        .expect("adjustment"));
    assert!(app
        .store
        .record_rating_change(
            &mut tx,
            app.channel,
            None,
            &delta,
            "penalty",
            Some(UserId(9))
        )
        .await
        .expect("second adjustment"));
    tx.commit().await.expect("commit");
}

#[tokio::test]
async fn expired_queue_members_are_swept_and_the_sweep_is_idempotent() {
    let url = require_database!();
    let app = TestApp::start(&url).await;
    let queue = app
        .with_queue(QueueSettings {
            autostart: false,
            ..QueueSettings::default()
        })
        .await;

    let now = app.now();
    app.store
        .add_queue_member(queue.id, UserId(1), now, Some(now + Duration::minutes(5)))
        .await
        .expect("expiring member");
    app.store
        .add_queue_member(queue.id, UserId(2), now, None)
        .await
        .expect("permanent member");

    let queues = QueueService::new(app.app.clone());
    assert_eq!(queues.sweep_expired().await.expect("early sweep"), 0);

    app.clock.advance(Duration::minutes(6));
    assert_eq!(queues.sweep_expired().await.expect("sweep"), 1);
    assert_eq!(
        queues.sweep_expired().await.expect("second sweep"),
        0,
        "the sweep must be idempotent"
    );

    let members = app.store.queue_members(queue.id).await.expect("members");
    assert_eq!(members.len(), 1);
    assert_eq!(members[0].user, UserId(2));
}

#[tokio::test]
async fn a_check_in_that_times_out_returns_players_to_the_queue() {
    let url = require_database!();
    let app = TestApp::start(&url).await;
    let queue = app
        .with_queue(QueueSettings {
            size: 2,
            autostart: true,
            team_formation: TeamFormationMode::NoTeams,
            check_in: Some(CheckInSettings {
                timeout_seconds: 60,
                abort_on_decline: false,
                return_policy: CheckInReturnPolicy::ReadyAndPending,
            }),
            ..QueueSettings::default()
        })
        .await;

    let queues = QueueService::new(app.app.clone());
    let context = queues.context(app.channel).await.expect("context");
    queues
        .add(&context, UserId(1), &[], None)
        .await
        .expect("join");
    let joined = queues
        .add(&context, UserId(2), &[], None)
        .await
        .expect("join");
    let match_id = joined.started.expect("autostart fired");

    let loaded = app.store.require_match(match_id).await.expect("load");
    assert_eq!(loaded.info.state, MatchState::CheckIn);
    assert!(
        app.store.queue_members(queue.id).await.unwrap().is_empty(),
        "launching must empty the queue"
    );

    // Nobody presses ready.
    app.clock.advance(Duration::seconds(61));
    MatchService::new(app.app.clone())
        .process_due()
        .await
        .expect("timer sweep");

    let loaded = app.store.require_match(match_id).await.expect("load");
    assert_eq!(loaded.info.state, MatchState::Cancelled);
    let members = app.store.queue_members(queue.id).await.expect("members");
    assert_eq!(
        members.len(),
        2,
        "both silent players go back into the queue"
    );
}

#[tokio::test]
async fn a_check_in_everyone_passes_moves_on_to_the_match() {
    let url = require_database!();
    let app = TestApp::start(&url).await;
    app.with_queue(QueueSettings {
        size: 2,
        autostart: true,
        team_formation: TeamFormationMode::NoTeams,
        check_in: Some(CheckInSettings::default()),
        ..QueueSettings::default()
    })
    .await;

    let queues = QueueService::new(app.app.clone());
    let context = queues.context(app.channel).await.expect("context");
    queues
        .add(&context, UserId(1), &[], None)
        .await
        .expect("join");
    let joined = queues
        .add(&context, UserId(2), &[], None)
        .await
        .expect("join");
    let match_id = joined.started.expect("autostart fired");

    let matches = MatchService::new(app.app.clone());
    matches
        .set_ready(match_id, UserId(1), true)
        .await
        .expect("ready");
    assert_eq!(
        app.store.require_match(match_id).await.unwrap().info.state,
        MatchState::CheckIn,
        "one ready player is not enough"
    );
    matches
        .set_ready(match_id, UserId(2), true)
        .await
        .expect("ready");
    assert_eq!(
        app.store.require_match(match_id).await.unwrap().info.state,
        MatchState::Active
    );
}

#[tokio::test]
async fn restart_recovery_resolves_deadlines_that_passed_while_the_process_was_down() {
    let url = require_database!();
    let app = TestApp::start(&url).await;
    app.with_queue(QueueSettings {
        size: 2,
        autostart: true,
        team_formation: TeamFormationMode::NoTeams,
        match_lifetime_seconds: 120,
        ..QueueSettings::default()
    })
    .await;

    let queues = QueueService::new(app.app.clone());
    let context = queues.context(app.channel).await.expect("context");
    queues
        .add(&context, UserId(1), &[], None)
        .await
        .expect("join");
    let joined = queues
        .add(&context, UserId(2), &[], None)
        .await
        .expect("join");
    let match_id = joined.started.expect("autostart fired");
    assert_eq!(
        app.store.require_match(match_id).await.unwrap().info.state,
        MatchState::Active
    );

    // Simulate the process being down past the match lifetime, then restarting.
    app.clock.advance(Duration::minutes(5));
    let restarted = TestApp {
        app: app.app.clone(),
        store: app.store.clone(),
        clock: app.clock.clone(),
        guild: app.guild,
        channel: app.channel,
    };
    let live = restarted
        .store
        .all_live_matches()
        .await
        .expect("live matches survive a restart");
    assert_eq!(live.len(), 1, "the match is still in the database");

    pugbot::jobs::tick_once(&restarted.app).await;

    let loaded = restarted.store.require_match(match_id).await.expect("load");
    assert_eq!(
        loaded.info.state,
        MatchState::Expired,
        "an unreported ranked match expires rather than being invented"
    );
    assert!(!loaded.info.rated, "an expired match is never rated");
}

#[tokio::test]
async fn a_map_vote_resolves_when_everyone_has_voted() {
    let url = require_database!();
    let app = TestApp::start(&url).await;
    app.with_queue(QueueSettings {
        size: 2,
        autostart: true,
        team_formation: TeamFormationMode::NoTeams,
        maps: MapSettings {
            pool: vec!["alpha".into(), "bravo".into(), "charlie".into()],
            pick_count: 1,
            cooldown_matches: 0,
            vote: Some(MapVoteSettings {
                candidates: 3,
                tie_break: TieBreak::Deterministic,
            }),
        },
        ..QueueSettings::default()
    })
    .await;

    let queues = QueueService::new(app.app.clone());
    let context = queues.context(app.channel).await.expect("context");
    queues
        .add(&context, UserId(1), &[], None)
        .await
        .expect("join");
    let joined = queues
        .add(&context, UserId(2), &[], None)
        .await
        .expect("join");
    let match_id = joined.started.expect("autostart fired");

    let loaded = app.store.require_match(match_id).await.expect("load");
    assert_eq!(loaded.info.state, MatchState::MapVote);
    assert_eq!(loaded.info.map_candidates.len(), 3);

    let matches = MatchService::new(app.app.clone());
    matches
        .cast_vote(match_id, UserId(1), 1)
        .await
        .expect("vote");
    matches
        .cast_vote(match_id, UserId(2), 1)
        .await
        .expect("vote");

    let loaded = app.store.require_match(match_id).await.expect("load");
    assert_eq!(loaded.info.state, MatchState::Active);
    assert_eq!(loaded.info.maps.len(), 1);
    assert_eq!(
        loaded.info.maps[0], loaded.info.map_candidates[1],
        "the winning candidate must be the one that was voted for"
    );
}

#[tokio::test]
async fn a_captain_draft_persists_and_reloads_correctly() {
    let url = require_database!();
    let app = TestApp::start(&url).await;
    app.with_queue(QueueSettings {
        size: 4,
        autostart: true,
        team_formation: TeamFormationMode::CaptainDraft,
        captain_mode: pugbot::domain::settings::CaptainMode::Random,
        ..QueueSettings::default()
    })
    .await;

    let queues = QueueService::new(app.app.clone());
    let context = queues.context(app.channel).await.expect("context");
    let mut match_id = MatchId(0);
    for id in 1..=4i64 {
        let result = queues
            .add(&context, UserId(id), &[], None)
            .await
            .expect("join");
        if let Some(started) = result.started {
            match_id = started;
        }
    }
    assert_ne!(match_id, MatchId(0), "autostart fired");

    let loaded = app.store.require_match(match_id).await.expect("load");
    assert_eq!(loaded.info.state, MatchState::TeamFormation);
    let draft = loaded.draft().expect("draft rebuilds from rows");
    assert!(draft.captains_ready(), "two captains were appointed");
    assert_eq!(draft.pool.len(), 2, "two players are left to pick");

    let captain = draft.current_captain().expect("somebody is on the clock");
    let target = draft.pool[0];
    MatchService::new(app.app.clone())
        .pick(match_id, captain, target)
        .await
        .expect("pick");

    // Reloading from the database must show the same draft.
    let reloaded = app.store.require_match(match_id).await.expect("reload");
    let draft = reloaded.draft().expect("draft");
    assert!(draft.is_complete(), "the last player is auto-assigned");
    assert_eq!(reloaded.info.state, MatchState::Active);
    assert_eq!(reloaded.picks.len(), 2, "both picks were recorded");
}

#[tokio::test]
async fn a_disputed_result_waits_for_a_moderator() {
    let url = require_database!();
    let app = TestApp::start(&url).await;
    let queue = app.with_queue(ranked_queue(2)).await;

    let mut tx = app.store.begin().await.expect("begin");
    let id = app
        .store
        .create_match(
            &mut tx,
            &NewMatch {
                guild: app.guild,
                channel: app.channel,
                queue: Some(queue.id),
                state: MatchState::Active,
                ranked: true,
                rating_pool: app.channel,
                settings: ranked_queue(2),
                players: vec![UserId(1), UserId(2)],
                mode: "debug".to_string(),
                check_in_ends_at: None,
                expires_at: None,
            },
        )
        .await
        .expect("create");
    app.store
        .set_teams(
            &mut tx,
            id,
            &[vec![UserId(1)], vec![UserId(2)]],
            &[None, None],
        )
        .await
        .expect("teams");
    tx.commit().await.expect("commit");

    let matches = MatchService::new(app.app.clone());
    matches
        .report(id, UserId(1), ReportOutcome::Win(0))
        .await
        .expect("report");
    matches
        .report(id, UserId(2), ReportOutcome::Win(1))
        .await
        .expect("contradicting report");

    let loaded = app.store.require_match(id).await.expect("load");
    assert_eq!(loaded.info.state, MatchState::ReportPending);
    assert!(!loaded.info.rated, "a disputed match is not rated");

    matches
        .moderator_report(id, UserId(99), ReportOutcome::Win(1), Some(vec![13, 16]))
        .await
        .expect("moderator settles it");

    let loaded = app.store.require_match(id).await.expect("load");
    assert_eq!(loaded.info.state, MatchState::Completed);
    assert_eq!(loaded.info.winner_team, Some(1));
    assert_eq!(loaded.info.scores, Some(vec![13, 16]));
    assert!(loaded.info.rated);
}

#[tokio::test]
async fn a_shared_rating_pool_is_written_once_for_both_channels() {
    let url = require_database!();
    let app = TestApp::start(&url).await;
    let other = ChannelId(900_000_000_000_000_003);

    let mut shared = pugbot::domain::settings::ChannelSettings::default();
    shared.rating.system = RatingSystemKind::Flat;
    app.with_queue_and_channel(ranked_queue(2), shared.clone())
        .await;

    // The second channel points its ratings at the first.
    let mut pooled = shared.clone();
    pooled.rating_pool_channel_id = Some(app.channel);
    app.store
        .enable_channel(app.guild, other, &pooled)
        .await
        .expect("enable second channel");

    let config = app
        .store
        .require_enabled_channel(other)
        .await
        .expect("load config");
    assert_eq!(
        config.rating_pool(),
        app.channel,
        "the second channel reads the first channel's pool"
    );

    let stats = app
        .store
        .player_stats_bulk(config.rating_pool(), &[UserId(1)], &RatingConfig::default())
        .await
        .expect("stats");
    assert_eq!(stats.len(), 1);
    assert_eq!(stats[0].channel, app.channel);
}

#[tokio::test]
async fn job_leases_stop_two_workers_running_the_same_sweep() {
    let url = require_database!();
    let app = TestApp::start(&url).await;
    let now = app.now();
    let until = now + Duration::seconds(60);

    assert!(app
        .store
        .try_acquire_job_lock("timers", "debug", "worker-a", until, now)
        .await
        .expect("first lease"));
    assert!(
        !app.store
            .try_acquire_job_lock("timers", "debug", "worker-b", until, now)
            .await
            .expect("contended lease"),
        "a second worker must not get the lease while it is held"
    );
    assert!(
        app.store
            .try_acquire_job_lock("timers", "debug", "worker-a", until, now)
            .await
            .expect("renewal"),
        "the holder must be able to renew its own lease"
    );
    assert!(
        app.store
            .try_acquire_job_lock("timers", "production", "worker-b", until, now)
            .await
            .expect("other mode"),
        "the other mode has an independent lease"
    );

    // Once the lease expires anybody may take it.
    let later = now + Duration::seconds(61);
    assert!(app
        .store
        .try_acquire_job_lock(
            "timers",
            "debug",
            "worker-b",
            later + Duration::seconds(60),
            later
        )
        .await
        .expect("expired lease"));
}

#[tokio::test]
async fn moderation_actions_are_audited_with_their_mode() {
    let url = require_database!();
    let app = TestApp::start(&url).await;
    app.with_queue(QueueSettings::default()).await;

    app.app
        .audit(
            Some(app.guild),
            Some(app.channel),
            Some(UserId(7)),
            "moderation.ban",
            Some("42"),
            serde_json::json!({ "reason": "testing" }),
        )
        .await;

    let events = app
        .store
        .recent_audit_events(app.guild, 10)
        .await
        .expect("audit events");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].action, "moderation.ban");
    assert_eq!(events[0].actor, Some(UserId(7)));

    let mode: String = sqlx::query_scalar("SELECT mode FROM audit_events LIMIT 1")
        .fetch_one(app.store.pool())
        .await
        .expect("mode column");
    assert_eq!(mode, "debug", "every audit row records the running mode");
}

#[tokio::test]
async fn an_active_ban_blocks_joining_until_it_is_released() {
    let url = require_database!();
    let app = TestApp::start(&url).await;
    app.with_queue(QueueSettings {
        autostart: false,
        ..QueueSettings::default()
    })
    .await;

    let now = app.now();
    app.store
        .add_queue_ban(
            app.guild,
            UserId(1),
            UserId(9),
            Some("testing"),
            now + Duration::hours(1),
        )
        .await
        .expect("ban");

    let queues = QueueService::new(app.app.clone());
    let context = queues.context(app.channel).await.expect("context");
    let blocked = queues.add(&context, UserId(1), &[], None).await;
    assert!(matches!(
        blocked,
        Err(ServiceError::Domain(DomainError::QueueBanned { .. }))
    ));

    app.store
        .release_bans(app.guild, UserId(1), UserId(9))
        .await
        .expect("release");
    queues
        .add(&context, UserId(1), &[], None)
        .await
        .expect("joining works once the ban is lifted");
}
