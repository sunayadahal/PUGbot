-- PUGbot initial schema.
--
-- Design notes:
--   * Anything queried, joined, sorted or constrained gets a real column.
--     JSONB is used only for configuration blobs (which are read whole) and
--     for the immutable per-match settings snapshot.
--   * The single-queue-per-channel rule, the one-live-match-per-player rule,
--     and the rate-once-per-match rule are enforced by the database, not only
--     by the service layer.

CREATE TABLE guilds (
    guild_id    BIGINT      PRIMARY KEY,
    enabled     BOOLEAN     NOT NULL DEFAULT TRUE,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE channel_configs (
    channel_id  BIGINT      PRIMARY KEY,
    guild_id    BIGINT      NOT NULL REFERENCES guilds (guild_id) ON DELETE CASCADE,
    enabled     BOOLEAN     NOT NULL DEFAULT TRUE,
    -- Serialised domain::settings::ChannelSettings.
    settings    JSONB       NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX channel_configs_guild_idx ON channel_configs (guild_id);

-- One queue per enabled channel: the UNIQUE constraint on channel_id is the
-- enforcement point the specification asks for.
CREATE TABLE queues (
    queue_id         BIGSERIAL   PRIMARY KEY,
    channel_id       BIGINT      NOT NULL UNIQUE
                                 REFERENCES channel_configs (channel_id) ON DELETE CASCADE,
    guild_id         BIGINT      NOT NULL,
    -- Serialised domain::settings::QueueSettings.
    settings         JSONB       NOT NULL,
    last_promoted_at TIMESTAMPTZ,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE queue_members (
    queue_id   BIGINT      NOT NULL REFERENCES queues (queue_id) ON DELETE CASCADE,
    user_id    BIGINT      NOT NULL,
    joined_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ,
    PRIMARY KEY (queue_id, user_id)
);

CREATE INDEX queue_members_expiry_idx
    ON queue_members (expires_at)
    WHERE expires_at IS NOT NULL;

CREATE TABLE users (
    user_id                BIGINT      PRIMARY KEY,
    dm_on_start            BOOLEAN     NOT NULL DEFAULT TRUE,
    default_expiry_seconds BIGINT,
    allow_offline_until    TIMESTAMPTZ,
    auto_ready_until       TIMESTAMPTZ,
    updated_at             TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Ratings live per rating pool. The pool is normally the channel itself, but a
-- channel may point at another channel's pool, so there is deliberately no
-- foreign key: deleting a channel must not delete shared rating history.
CREATE TABLE channel_players (
    channel_id           BIGINT           NOT NULL,
    user_id              BIGINT           NOT NULL,
    rating               DOUBLE PRECISION NOT NULL,
    deviation            DOUBLE PRECISION NOT NULL,
    volatility           DOUBLE PRECISION NOT NULL,
    wins                 INTEGER          NOT NULL DEFAULT 0,
    losses               INTEGER          NOT NULL DEFAULT 0,
    draws                INTEGER          NOT NULL DEFAULT 0,
    streak               INTEGER          NOT NULL DEFAULT 0,
    hidden               BOOLEAN          NOT NULL DEFAULT FALSE,
    last_ranked_match_at TIMESTAMPTZ,
    created_at           TIMESTAMPTZ      NOT NULL DEFAULT now(),
    updated_at           TIMESTAMPTZ      NOT NULL DEFAULT now(),
    PRIMARY KEY (channel_id, user_id)
);

CREATE INDEX channel_players_leaderboard_idx
    ON channel_players (channel_id, rating DESC)
    WHERE hidden = FALSE;

CREATE TABLE matches (
    match_id               BIGSERIAL   PRIMARY KEY,
    guild_id               BIGINT      NOT NULL,
    channel_id             BIGINT      NOT NULL,
    queue_id               BIGINT      REFERENCES queues (queue_id) ON DELETE SET NULL,
    state                  TEXT        NOT NULL,
    -- Bumped on every transition; used for optimistic locking.
    version                INTEGER     NOT NULL DEFAULT 1,
    ranked                 BOOLEAN     NOT NULL,
    rating_pool_channel_id BIGINT      NOT NULL,
    -- Effective QueueSettings at launch. Later queue edits must not change how
    -- a historical match is interpreted.
    settings               JSONB       NOT NULL,
    map_candidates         JSONB       NOT NULL DEFAULT '[]'::jsonb,
    maps                   JSONB       NOT NULL DEFAULT '[]'::jsonb,
    scores                 JSONB,
    -- Winning team index; NULL for a draw, cancellation or unfinished match.
    winner_team            INTEGER,
    outcome                TEXT,
    -- Set exactly once, when ratings have been written.
    rated                  BOOLEAN     NOT NULL DEFAULT FALSE,
    mode                   TEXT        NOT NULL,
    check_in_ends_at       TIMESTAMPTZ,
    vote_ends_at           TIMESTAMPTZ,
    expires_at             TIMESTAMPTZ,
    created_at             TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at             TIMESTAMPTZ NOT NULL DEFAULT now(),
    started_at             TIMESTAMPTZ,
    finished_at            TIMESTAMPTZ,
    CONSTRAINT matches_state_valid CHECK (state IN (
        'QUEUED', 'CHECK_IN', 'TEAM_FORMATION', 'MAP_VOTE',
        'ACTIVE', 'REPORT_PENDING', 'COMPLETED', 'CANCELLED', 'EXPIRED'
    ))
);

CREATE INDEX matches_channel_idx ON matches (channel_id, created_at DESC);
CREATE INDEX matches_live_idx
    ON matches (state)
    WHERE state NOT IN ('COMPLETED', 'CANCELLED', 'EXPIRED');
CREATE INDEX matches_timers_idx
    ON matches (check_in_ends_at, vote_ends_at, expires_at)
    WHERE state NOT IN ('COMPLETED', 'CANCELLED', 'EXPIRED');

CREATE TABLE match_players (
    match_id         BIGINT           NOT NULL REFERENCES matches (match_id) ON DELETE CASCADE,
    user_id          BIGINT           NOT NULL,
    -- Denormalised from the match so the live-participation index below can be
    -- a plain partial unique index.
    channel_id       BIGINT           NOT NULL,
    live             BOOLEAN          NOT NULL DEFAULT TRUE,
    team             INTEGER,
    is_captain       BOOLEAN          NOT NULL DEFAULT FALSE,
    ready_state      TEXT             NOT NULL DEFAULT 'pending',
    joined_at        TIMESTAMPTZ      NOT NULL DEFAULT now(),
    -- The player this one replaced, if they came in as a substitute.
    substituted_for  BIGINT,
    rating_before    DOUBLE PRECISION,
    deviation_before DOUBLE PRECISION,
    PRIMARY KEY (match_id, user_id),
    CONSTRAINT match_players_ready_state_valid
        CHECK (ready_state IN ('pending', 'ready', 'declined'))
);

-- A player belongs to at most one live match per channel.
CREATE UNIQUE INDEX match_players_one_live_per_channel_idx
    ON match_players (channel_id, user_id)
    WHERE live;

CREATE INDEX match_players_user_idx ON match_players (user_id);

CREATE TABLE draft_picks (
    id              BIGSERIAL   PRIMARY KEY,
    match_id        BIGINT      NOT NULL REFERENCES matches (match_id) ON DELETE CASCADE,
    seq             INTEGER     NOT NULL,
    team            INTEGER     NOT NULL,
    -- NULL for the automatic assignment of the last remaining player.
    captain_user_id BIGINT,
    player_user_id  BIGINT      NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (match_id, seq)
);

CREATE TABLE map_votes (
    match_id        BIGINT      NOT NULL REFERENCES matches (match_id) ON DELETE CASCADE,
    user_id         BIGINT      NOT NULL,
    candidate_index INTEGER     NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (match_id, user_id)
);

CREATE TABLE match_reports (
    match_id    BIGINT      NOT NULL REFERENCES matches (match_id) ON DELETE CASCADE,
    user_id     BIGINT      NOT NULL,
    outcome     TEXT        NOT NULL,
    winner_team INTEGER,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (match_id, user_id),
    CONSTRAINT match_reports_outcome_valid CHECK (outcome IN ('win', 'draw', 'cancel'))
);

CREATE TABLE rating_history (
    id                BIGSERIAL        PRIMARY KEY,
    channel_id        BIGINT           NOT NULL,
    user_id           BIGINT           NOT NULL,
    match_id          BIGINT           REFERENCES matches (match_id) ON DELETE SET NULL,
    rating_before     DOUBLE PRECISION NOT NULL,
    rating_after      DOUBLE PRECISION NOT NULL,
    deviation_before  DOUBLE PRECISION NOT NULL,
    deviation_after   DOUBLE PRECISION NOT NULL,
    reason            TEXT             NOT NULL,
    -- The moderator or administrator responsible, for manual adjustments.
    actor_id          BIGINT,
    created_at        TIMESTAMPTZ      NOT NULL DEFAULT now()
);

CREATE INDEX rating_history_player_idx ON rating_history (channel_id, user_id, created_at DESC);

-- Ratings are generated exactly once per player per finalised match. A retried
-- or concurrent finalisation hits this index instead of double-rating.
CREATE UNIQUE INDEX rating_history_once_per_match_idx
    ON rating_history (match_id, user_id)
    WHERE match_id IS NOT NULL;

CREATE TABLE queue_bans (
    id          BIGSERIAL   PRIMARY KEY,
    guild_id    BIGINT      NOT NULL,
    user_id     BIGINT      NOT NULL,
    issuer_id   BIGINT      NOT NULL,
    reason      TEXT,
    started_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at  TIMESTAMPTZ NOT NULL,
    released_at TIMESTAMPTZ,
    released_by BIGINT
);

CREATE INDEX queue_bans_active_idx
    ON queue_bans (guild_id, user_id, expires_at)
    WHERE released_at IS NULL;

CREATE TABLE player_phrases (
    id         BIGSERIAL   PRIMARY KEY,
    channel_id BIGINT      NOT NULL,
    user_id    BIGINT      NOT NULL,
    phrase     TEXT        NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX player_phrases_lookup_idx ON player_phrases (channel_id, user_id);

CREATE TABLE subscriptions (
    channel_id BIGINT      NOT NULL,
    user_id    BIGINT      NOT NULL,
    role_id    BIGINT      NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (channel_id, user_id, role_id)
);

CREATE TABLE audit_events (
    id         BIGSERIAL   PRIMARY KEY,
    guild_id   BIGINT,
    channel_id BIGINT,
    actor_id   BIGINT,
    action     TEXT        NOT NULL,
    target     TEXT,
    data       JSONB       NOT NULL DEFAULT '{}'::jsonb,
    -- Every audit row records which mode produced it.
    mode       TEXT        NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX audit_events_guild_idx ON audit_events (guild_id, created_at DESC);
CREATE INDEX audit_events_action_idx ON audit_events (action, created_at DESC);

-- Background jobs take a lease here so two processes (or the debug and
-- production processes) never run the same sweep at the same time. The mode is
-- part of the key, so debug and production leases can never collide.
CREATE TABLE job_locks (
    name         TEXT        NOT NULL,
    mode         TEXT        NOT NULL,
    holder       TEXT        NOT NULL,
    locked_until TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (name, mode)
);
