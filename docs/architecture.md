# Architecture description

## Scope

This document describes PUGbot's software architecture for developers,
reviewers, and operators who need to reason about behaviour beyond the API
reference. It follows the ISO/IEC/IEEE 42010 pattern: stakeholders, their
concerns, and a viewpoint addressing each.

## Stakeholders and concerns

| Stakeholder | Concerns |
| --- | --- |
| Player | Joining takes one action; the bot does not lose my place; a result cannot be falsified against me |
| Server administrator | Each channel configures independently; destructive actions are confirmed and audited |
| Moderator | Recovery tools exist when a match goes wrong |
| Operator | Restart safety; observability; debug work cannot touch production |
| Developer | Rules are testable without Discord or a database; invariants are enforced, not merely intended |

## Context

```text
        Discord Gateway ──┐            ┌── Discord HTTP API
                          ▼            ▲
                    ┌─────────────────────────┐
   Prometheus ◀──── │        PUGbot           │ ────▶ PostgreSQL
   /metrics         │  (one process, one mode)│
   Kubernetes ◀──── └─────────────────────────┘
   /health /ready
```

One process serves many guilds and channels. It holds no durable state in
memory: everything that must survive a restart is in PostgreSQL.

---

## Viewpoint 1 — Module decomposition

**Concern:** can rules be tested without Discord or a database?

```text
discord      adapters: slash commands, components, embeds, gateway events
   │
services     use cases, transaction boundaries, permission enforcement
   │
domain       pure rules, state machines, invariants — no I/O
   │
repositories PostgreSQL persistence, schema-enforced invariants
```

Dependencies point downward only. `domain` is the innermost layer and depends on
nothing but `error` and `ids`.

| Module | Responsibility | Must not |
| --- | --- | --- |
| `domain` | Rules, state machines, invariants | Perform I/O, read the wall clock, name a Discord type |
| `services` | Use cases, transactions, permissions | Call Discord directly, or hold a transaction across a network call |
| `repositories` | Persistence | Contain rules that belong in `domain` |
| `discord` | Translate interactions to service calls, render results | Contain rules of its own |
| `jobs` | Periodic sweeps | Duplicate transition logic — it calls `advance` like everything else |
| `config` | Load and validate configuration; enforce mode isolation | Read the other mode's variables |
| `localization` | Message catalogs | Contain formatting logic beyond substitution |
| `observability` | Logging, metrics, health | Serve anything that could carry a credential |

### Consequences of this shape

The 211 unit tests need no database and no network. The domain rules that are
hardest to get right — draft termination for any pick order, check-in return
policy, rating arithmetic, consensus — are pure functions and are tested
exhaustively as such.

The cost is indirection: adding a setting touches the domain struct, the patch
struct, the command definition, and the handler. That is deliberate. Each of
those layers rejects a different class of mistake.

---

## Viewpoint 2 — Match lifecycle

**Concern:** can a restart, a retry, or a double-click corrupt a match?

```text
QUEUED ─┬─▶ CHECK_IN ─┬─▶ TEAM_FORMATION ─┬─▶ MAP_VOTE ─┬─▶ ACTIVE
        │             │                   │            │      │
        └─────────────┴───────────────────┴────────────┴──────┼──▶ CANCELLED
                                                              ├──▶ EXPIRED
                                                              ▼
                                              REPORT_PENDING ◀─┴─▶ COMPLETED
```

Every legal transition is declared in one place, `MatchState::allowed_next`.
Every transition is *applied* in one place, `MatchService::advance`, which
pushes a match as far forward as the current facts allow and then stops.

Commands change a fact — a ready press, a pick, a vote — and then call
`advance`. The timer job calls it too. There is no separate code path for
"resuming" a match, because resumption is the same call.

### Why this makes retries safe

Each step re-reads the match and re-checks its preconditions. So:

* A double-clicked button: the second press finds the state already advanced and
  does nothing.
* A retried job: same.
* A process restart mid-draft: the draft is rebuilt from `match_players` and
  `draft_picks`, and `advance` continues from there.
* Two workers racing: `matches.version` is checked on update, and the loser gets
  `ServiceError::Conflict`.

### Why an expired match is never rated

A ranked match that outlives its configured lifetime becomes `EXPIRED`, not
`COMPLETED`. The bot has no evidence of what happened; inventing a result would
corrupt every participant's rating. This is asserted by
`restart_recovery_resolves_deadlines_that_passed_while_the_process_was_down`.

---

## Viewpoint 3 — Invariant enforcement

**Concern:** are the critical rules actually enforced, or merely intended?

Four invariants are enforced by the database, so a service bug, a race, or a
future refactor cannot violate them silently.

| Invariant | Mechanism | Verified by |
| --- | --- | --- |
| One queue per channel | `UNIQUE (channel_id)` on `queues` | `a_channel_can_only_ever_own_one_queue` |
| One live match per player per channel | Partial unique index on `match_players (channel_id, user_id) WHERE live` | `a_player_cannot_be_in_two_live_matches_in_one_channel` |
| Ratings applied exactly once per match | Partial unique index on `rating_history (match_id, user_id)`, plus the one-shot `matches.rated` flag claimed inside the finalising transaction | `a_ranked_match_is_rated_exactly_once`, `the_rating_history_index_blocks_a_duplicate_write` |
| Queue capacity under concurrency | Row lock on the queue inside `add_queue_member_atomic` | `concurrent_joins_never_exceed_the_queue_size` |

The last one was added after the integration test caught the original
implementation overfilling a four-slot queue with ten simultaneous joins: the
capacity check and the insert were separate statements. Checking in the service
and inserting afterwards is not enough, and the test now proves it.

Two further invariants live in the domain and are property-tested rather than
schema-enforced, because they concern a single in-memory value:

* A draft terminates with full teams for every pick order —
  `every_pick_order_terminates_with_full_teams`.
* Check-in failure lists partition the roster exactly —
  `failure_lists_partition_the_roster`.

---

## Viewpoint 4 — Mode isolation

**Concern:** can development work touch production?

Debug and production are separate configurations of one binary, selected by a
required argument with no default.

```text
PUGBOT_DEBUG_*                      PUGBOT_PRODUCTION_*
      │                                     │
      └──────────┐             ┌────────────┘
                 ▼             ▼
            AppConfig::load(mode)
                 │
                 ├─ reads only this mode's prefix
                 ├─ debug requires a guild allowlist
                 ├─ refuses a shared token
                 ├─ refuses a shared database
                 └─ refuses a guild in both allowlists
                 ▼
            startup banner, then connect
```

There is no fallback path between modes: a missing debug value is an error, not
a reason to read the production one. This is structural — nothing in the process
reads the other mode's prefix except the cross-check that refuses to start.

| Concern | Debug | Production |
| --- | --- | --- |
| Guild allowlist | Required, enforced on every interaction | Optional; empty means any guild |
| Command registration | Per guild — instant updates | Global |
| Logs | Human-readable with source locations | JSON |
| Reset tooling | Available with `--features debug-tools` | Cannot be compiled in |
| Job leases | Keyed `(name, "debug")` | Keyed `(name, "production")` |
| Audit rows | `mode = 'debug'` | `mode = 'production'` |

The debug reset command is guarded three times over: a Cargo feature, a runtime
mode check, and an explicit `--yes-delete-everything` flag. A production binary
built without the feature does not contain the subcommand at all, which CI
asserts by inspecting the release binary.

---

## Viewpoint 5 — Concurrency and failure

**Concern:** what happens when things overlap or break?

### Transaction boundaries

Services own transactions. The rule that shapes the code is: **no transaction is
held across a Discord network call.** Discord can be slow or unavailable, and a
held transaction would hold a row lock for that whole time.

So `MatchService::finalize` writes the outcome, transitions the state, claims
the rating flag, and applies ratings in one transaction — then commits, and only
then announces.

### Background jobs

Two loops, each leased through `job_locks` keyed by `(name, mode)`:

| Job | Interval | Lease | Work |
| --- | --- | --- | --- |
| `timers` | 15 s | 60 s | Sweep expired queue slots; advance matches past a deadline |
| `decay` | 1 h | 60 s | Apply inactivity rating decay |

The lease outlives a tick — asserted by `the_lease_outlives_a_tick` — so a slow
sweep does not lose it mid-run. The holder can always renew. Missed ticks are
delayed rather than bursting, so a stalled process does not produce a thundering
catch-up.

### Failure handling

| Failure | Response |
| --- | --- |
| Database unreachable | Commands fail with a generic message; `/ready` returns 503; `/health` still returns 200, so the process is not restarted for a dependency outage |
| Discord rate limit or outage | The notifier logs and drops; match state is already committed |
| Player has DMs closed | Logged at debug; the match starts regardless |
| Two workers race a transition | The loser gets `Conflict`; `process_due` swallows it as expected |
| Audit write fails | Logged as an error; the moderator's command still succeeds — losing one audit row is bad, failing the command is worse |
| Unknown component identifier | Refused with a clear message rather than acted on, so a button from an older deployment cannot hit the wrong match |

---

## Viewpoint 6 — Data

See [`data-model.md`](data-model.md) for the schema. The architectural decisions
about data are:

* **Relational where it is queried; JSONB where it is read whole.** Anything
  joined, sorted, or constrained is a real column. JSONB holds only the settings
  blobs and the per-match settings snapshot.
* **Settings are snapshotted onto each match.** Later edits to a queue must not
  change how a historical match is interpreted.
* **Ratings live in a pool, not a channel.** A channel normally owns its pool but
  may share another channel's, so two queues can feed one ladder.
* **History is append-only.** Corrections write new rows — a released ban, a new
  `rating_history` entry — rather than overwriting.

---

## Decision record

The specification left eight decisions open. Each is recorded with its rationale
and its cost.

### 1. Serenity and PostgreSQL; runtime SQL rather than macros

Serenity has the broader Rust ecosystem presence. PostgreSQL provides the
partial unique indexes that three of the four schema-enforced invariants rely
on.

Queries use `sqlx`'s runtime API rather than the compile-time macros, so
`cargo build` and `cargo clippy` work without a live database. The cost is that
SQL errors surface at test time rather than compile time; the mitigation is the
integration suite, which exercises every query against real PostgreSQL.

### 2. Slash commands only

No message-prefix compatibility layer. Slash commands give typed options,
per-command permissions, and discoverability. The cost is that very old clients
cannot use the bot.

### 3. Queue scope is configurable, defaulting to guild

`/channel set queue-scope:` chooses whether a live match anywhere in the server
blocks queueing, or only one in the same channel. Guild is the default because
a player in two simultaneous matches is almost always a mistake.

### 4. Ready-check failure and match expiry

Check-in failure cancels the match. Who returns to the queue is configurable —
`ready_only`, `ready_and_pending`, `none` — but a player who *declined* is never
returned, under any policy. Expiry ends a match `EXPIRED` and unrated.

### 5. Consensus, with moderator override

One report from each team finalises. Disagreement — between teams or between
teammates — moves the match to `REPORT_PENDING`, where only a moderator can
settle it. This keeps ratings out of the hands of any single player. Corrections
are audited rather than overwriting silently.

### 6. All three rating systems

Flat is the default because it is the easiest to explain in a Discord channel.
Glicko-2 and TrueSkill are implemented in full, including Glicko-2's volatility
iteration and TrueSkill's draw margin. Each is a `RatingSystem` implementation,
so a fourth is additive.

### 7. No web configuration UI

Configuration is entirely through slash commands. A web UI would need its own
authentication, session handling, and deployment surface, for configuration that
is already expressible as commands.

### 8. No GPL code copied

Behaviour was reimplemented from the specification. PUBobot2's source was not
consulted for implementation, so no GPL obligations attach to this work.

---

## Known limitations

Stated here rather than discovered later:

* **Never run against a live Discord gateway.** Command registration and
  interaction dispatch compile and are unit-tested; the first real connection is
  unproven.
* **Three-or-more-team balance is best-effort.** Two teams are solved exactly by
  exhaustive search up to 20 players. Beyond that, and for more teams, a greedy
  pass with pairwise swap refinement can stop at a local optimum a two-swap
  sequence would escape. Documented on `balanced_teams` and asserted honestly in
  its test.
* **`/nick` reports the prefix rather than applying it.** Writing a nickname
  needs a gateway call from the adapter.
* **Presence-based removal needs a privileged intent.** Without
  `GUILD_PRESENCES` granted, Discord never sends the event and the feature is
  inert rather than broken.
* **Single process.** Two replicas would coexist safely — the job leases and
  optimistic locking are built for it — but this has not been tested.
