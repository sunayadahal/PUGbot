# Data model

## Scope

The PostgreSQL schema: what each table holds, why it is shaped that way, and
which constraints carry real weight. The authoritative definition is
[`migrations/0001_initial.sql`](../migrations/0001_initial.sql).

## Design rules

1. **Relational where it is queried; JSONB where it is read whole.** Anything
   joined, sorted, filtered, or constrained is a real column. JSONB holds only
   the settings blobs and the per-match settings snapshot, which are always read
   in their entirety.
2. **Invariants belong in the schema.** Where a constraint can express a rule,
   it does — so a service bug or a race cannot violate it silently.
3. **History is append-only.** Corrections add rows; they do not overwrite.
4. **Migrations, never runtime mutation.** Structure changes ship as migrations.

## Entity overview

```text
guilds ──< channel_configs ──< queues ──< queue_members
                  │              │
                  │              └──< matches ──< match_players
                  │                      │   ├──< draft_picks
                  │                      │   ├──< map_votes
                  │                      │   └──< match_reports
                  └── channel_players     └──< rating_history
                      player_phrases
                      subscriptions

users            (cross-channel player preferences)
queue_bans       (guild-wide, timed)
audit_events     (append-only)
job_locks        (background job leases)
```

## Tables

### `guilds`

Discord servers the bot has seen. An unknown guild counts as enabled, so the bot
works the moment it is invited.

### `channel_configs`

One row per channel the bot has ever been enabled in. `settings` is a serialised
`ChannelSettings`: locale, roles, presence rules, rating configuration, rank
tiers, leaderboard rules, queue scope.

Disabling a channel sets `enabled = FALSE` rather than deleting the row, so
configuration, ratings, and history survive.

### `queues`

**The single-queue-per-channel rule lives here:**

```sql
channel_id BIGINT NOT NULL UNIQUE REFERENCES channel_configs (channel_id) ON DELETE CASCADE
```

`UNIQUE` makes a race between two administrators resolve into one queue and one
clean rejection, rather than two queues. `settings` is a serialised
`QueueSettings`.

### `queue_members`

Who is waiting. `PRIMARY KEY (queue_id, user_id)` makes duplicate membership
impossible. A partial index on `expires_at WHERE expires_at IS NOT NULL` keeps
the expiry sweep cheap.

Capacity is *not* a constraint — it depends on the queue's configured size — so
it is enforced by taking a row lock on the queue during a join. See
`Store::add_queue_member_atomic`.

### `users`

Cross-channel player preferences: DM opt-in, default expiry, offline
retention, auto-ready arming. A player who has never changed a setting has no
row; defaults are supplied in code.

### `channel_players`

Ratings, keyed by **rating pool** rather than by channel. A channel normally
owns its pool, but may point at another channel's, so two queues can feed one
ladder.

There is deliberately **no foreign key** to `channel_configs`: deleting a channel
must not cascade away a shared pool that another channel still uses.

A partial index supports the leaderboard:

```sql
CREATE INDEX channel_players_leaderboard_idx
    ON channel_players (channel_id, rating DESC) WHERE hidden = FALSE;
```

### `matches`

The central table.

| Column | Purpose |
| --- | --- |
| `state` | Lifecycle state, with a `CHECK` constraint listing the nine valid values |
| `version` | Optimistic-locking token, bumped on every transition |
| `settings` | The queue settings **as they were at launch** |
| `rated` | One-shot flag; claimed inside the finalising transaction |
| `mode` | Which mode created the match, so debug data is always identifiable |
| `check_in_ends_at`, `vote_ends_at`, `expires_at` | Deadlines the timer job sweeps |

**Why settings are snapshotted:** an administrator who changes the team size next
week must not retroactively change how last week's match is interpreted.

Three indexes: by channel and recency, a partial index on live matches, and a
partial index on the three deadline columns — the timer job's work list.

### `match_players`

The roster. `channel_id` and `live` are denormalised from the match so this can
be a plain partial unique index:

```sql
CREATE UNIQUE INDEX match_players_one_live_per_channel_idx
    ON match_players (channel_id, user_id) WHERE live;
```

That is what makes "a player is in at most one live match per channel" a
guarantee rather than an intention. `live` is cleared when the match reaches a
terminal state, and when a player is substituted out.

`rating_before` and `deviation_before` snapshot the player's rating at match
start, so a result can be explained even if their rating moves elsewhere first.

### `draft_picks`

Append-only pick history. `UNIQUE (match_id, seq)` makes a retried write a
no-op instead of a duplicate pick. `captain_user_id` is null for the automatic
assignment of the last remaining player — nobody chose it.

### `map_votes`

One ballot per player per match; recasting replaces. Candidates live in
`matches.map_candidates`, so a historical vote can still be read back in full.

### `match_reports`

One report per player per match; re-reporting replaces. A `CHECK` constraint
restricts `outcome` to `win`, `draw`, `cancel`. Consensus is computed from these
rows rather than stored, so the rule can change without a migration.

### `rating_history`

Append-only. Every change — from a match, a moderator adjustment, or decay —
writes a row.

**This is where the rate-once rule is enforced:**

```sql
CREATE UNIQUE INDEX rating_history_once_per_match_idx
    ON rating_history (match_id, user_id) WHERE match_id IS NOT NULL;
```

The partial predicate matters: manual adjustments carry no `match_id` and are
therefore never deduplicated. Two identical penalties are two real events; two
identical match ratings are a bug.

### `queue_bans`

Guild-wide and timed. Bans are **released**, not deleted, so who lifted one and
when stays on the record. Overlapping bans behave as one — the active expiry is
the maximum across unreleased rows.

### `player_phrases`, `subscriptions`

Per-channel join phrases, and promotion-role subscriptions.

### `audit_events`

Append-only. Every configuration change, moderator action, ban, reset, and
rating adjustment. `data` carries structured detail including before and after
values for edits, and `mode` records which mode produced the row.

### `job_locks`

Background job leases, keyed `(name, mode)`. The mode in the key is why a debug
process can never take production's lease.

## Constraint summary

| Constraint | Table | Guarantees |
| --- | --- | --- |
| `UNIQUE (channel_id)` | `queues` | One queue per channel |
| `PRIMARY KEY (queue_id, user_id)` | `queue_members` | No duplicate queue membership |
| `... (channel_id, user_id) WHERE live` | `match_players` | One live match per player per channel |
| `... (match_id, user_id) WHERE match_id IS NOT NULL` | `rating_history` | Ratings applied once per match |
| `UNIQUE (match_id, seq)` | `draft_picks` | No duplicate pick |
| `CHECK state IN (...)` | `matches` | No unknown lifecycle state |
| `CHECK ready_state IN (...)` | `match_players` | No unknown ready state |
| `CHECK outcome IN (...)` | `match_reports` | No unknown outcome |

## Migrations

Applied automatically at startup and by `pugbot --mode <mode> migrate`. `sqlx`
records applied migrations and their checksums, so an edited migration is
detected rather than silently skipped.

**To change the schema:** add a new file. Never edit an applied one.

```
migrations/
  0001_initial.sql
  0002_add_something.sql       ← new work goes here
```

Every integration test creates and migrates its own schema, so the migrations
are exercised from empty on every test run.

## Retention

Nothing is deleted automatically. Matches, rating history, and audit events
accumulate indefinitely — deliberately, since they are the evidence behind every
rating. A deployment that needs retention limits should archive
`audit_events` and completed `matches` on its own schedule, keeping
`rating_history` intact so ratings remain explainable.
