# PUGbot

A Discord bot for organising pickup games. Every PUG-enabled text channel owns
exactly one queue, and the bot takes that queue through check-in, team
formation, map selection, play, reporting, and rating updates.

Implemented in Rust against the specification in
[`FEATURE_IMPLEMENTATION_PLAN.md`](FEATURE_IMPLEMENTATION_PLAN.md).

## Quick start

```bash
# 1. A PostgreSQL database
createdb pugbot_debug

# 2. Configuration
cp .env.example .env.debug     # fill in the PUGBOT_DEBUG_* values

# 3. Schema
cargo run -- --mode debug --env-file .env.debug migrate

# 4. Check before connecting
cargo run -- --mode debug --env-file .env.debug check

# 5. Run
cargo run -- --mode debug --env-file .env.debug
```

Then, in Discord, as a server administrator:

```
/channel enable
/queue create name:pug size:10 teams:2
/add
```

## Modes

The mode is a required argument. There is no default, so a mistyped command
cannot start against production credentials.

| | Debug | Production |
| --- | --- | --- |
| Variables | `PUGBOT_DEBUG_*` | `PUGBOT_PRODUCTION_*` |
| Guild allowlist | Required; enforced on every interaction | Optional; empty means any guild |
| Command registration | Per guild — updates are instant | Global |
| Logs | Human-readable, with source locations | JSON, for aggregation |
| Reset tooling | Available with the `debug-tools` feature | Cannot be compiled in |

Startup refuses to continue if the two modes share a Discord token, share a
database URL, or list the same guild. Neither mode ever falls back to the
other's variables: a missing debug value is an error, not a reason to read the
production one. The startup banner prints the mode, application ID, database
host and name (without credentials), and the allowlisted guilds before anything
connects.

The debug-only reset command is gated three ways — a Cargo feature, a runtime
mode check, and an explicit acknowledgement flag:

```bash
cargo run --features debug-tools -- --mode debug --env-file .env.debug \
    debug-reset --yes-delete-everything
```

A production binary built without `--features debug-tools` does not contain the
command at all.

## Commands

| Area | Players | Staff |
| --- | --- | --- |
| Queue | `/add` `/remove` `/who` `/promote` `/subscribe` `/unsubscribe` `/server` `/maps` `/map` | `/queue create` `set-basics` `set-teams` `set-maps` `set-roles` `show` `delete` `add-player` `remove-player` `clear` `start` |
| Match | `/ready` `/notready` `/teams` `/matches` `/capfor` `/capme` `/pick` `/subme` `/subfor` `/report` | `/match report` `cancel` `sub-player` `put` `create` |
| Stats | `/rank` `/leaderboard` `/top` `/lastgame` `/stats show` `/nick` | `/rating seed` `penalty` `hide` `unhide` `snap`; `/stats reset` `reset-player` `replace-player` |
| Preferences | `/switch-dms` `/expire` `/expire-default` `/auto-ready` `/allow-offline` | — |
| Moderation | — | `/noadds list` `add` `remove`; `/phrases add` `clear` |
| Configuration | `/help` `/commands` | `/channel enable` `disable` `show` `set` |

Slash commands are the only interface; there are no message-prefix commands.
Check-in, map voting, and result reporting also work through buttons.

`/queue set` is split into `set-basics`, `set-teams`, `set-maps` and
`set-roles` because Discord allows at most 25 options per subcommand and the
full queue configuration has more than that. A test keeps the four groups
collectively exhaustive.

## Architecture

```
src/
  domain/         pure rules and state machines — no I/O
  services/       use cases and transaction boundaries
  repositories/   PostgreSQL persistence
  discord/        commands, components, embeds, gateway events
  jobs/           expiry, match timers, rating decay
  localization/   message catalogs and locale resolution
  config/         mode-separated configuration and validation
  observability/  logging, metrics, health endpoint
```

`domain` performs no I/O, reads no clock it was not given, and knows nothing
about Discord, so every rule is unit-testable in isolation. Services own
transactions; no transaction is held across a Discord network call.

### Match lifecycle

```
QUEUED → CHECK_IN → TEAM_FORMATION → MAP_VOTE → ACTIVE → REPORT_PENDING → COMPLETED
                                                                        ↘ CANCELLED
                                                                        ↘ EXPIRED
```

Every state change goes through one function, `MatchService::advance`, which
pushes a match as far forward as the current facts allow and stops. Commands
change a fact and call it; the timer job calls it too. Each step re-reads the
match and re-checks its preconditions, so a double-clicked button, a retried
job, and a restart are all no-ops rather than corruption.

### Invariants enforced by the database

These are schema constraints, not just service-layer checks:

- **One queue per channel** — `UNIQUE (channel_id)` on `queues`.
- **One live match per player per channel** — a partial unique index on
  `match_players (channel_id, user_id) WHERE live`.
- **Ratings applied exactly once per match** — a partial unique index on
  `rating_history (match_id, user_id)`, plus a one-shot `matches.rated` flag
  claimed inside the finalising transaction.
- **Queue capacity under concurrency** — joining takes a row lock on the queue,
  so the capacity check and the insert are one atomic step.

State transitions use optimistic locking on `matches.version`; a stale
transition returns a conflict instead of overwriting.

## Ratings

Three systems, selectable per channel with `/channel set rating-system:`:

- **Flat** (default) — a fixed change per result, with optional win/loss
  scaling, draw bonus, and streak multipliers.
- **Glicko-2** — the full algorithm including the volatility iteration. Each
  player is rated against an aggregate opponent formed from the other team.
- **TrueSkill** — the closed-form two-team update, with a draw margin derived
  from the configured draw probability.

Rank tiers can assign Discord roles and nickname prefixes. Inactivity decay
pulls ratings toward the configured baseline and restores deviation, and never
pushes an inactive player below a newcomer. Every change — from a match, an
admin adjustment, or decay — writes a `rating_history` row.

## Localisation

Eight catalogs ship embedded in the binary: English, French, Russian, Spanish,
Italian, Korean, Brazilian Portuguese, and Turkish. Set the language per channel
with `/channel set locale:`.

Tests enforce that every catalog has exactly the same keys as English and that
translations preserve every `{placeholder}`, so a missing or malformed
translation fails the build rather than reaching a player.

## Testing

```bash
cargo test                                     # unit tests; database tests skip
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
```

The database integration tests cover what only a real PostgreSQL instance can
prove: schema constraints, concurrent joins, transactional finalisation,
optimistic locking, restart recovery, and job leases. They skip with a message
unless a database is configured:

```bash
export PUGBOT_TEST_DATABASE_URL=postgres://user@localhost/pugbot_test
cargo test --all-features
```

Each test creates and migrates its own schema, so the suite runs in parallel
and every test also exercises the migrations from empty.

If you have no PostgreSQL superuser handy, a throwaway cluster needs no root:

```bash
export PATH=$PATH:/usr/lib/postgresql/17/bin
initdb -D /tmp/pgtest/data -U pugtest --auth=trust
pg_ctl -D /tmp/pgtest/data -o "-p 55432 -k /tmp/pgtest -c listen_addresses=''" \
       -l /tmp/pgtest/server.log start
createdb -h /tmp/pgtest -p 55432 -U pugtest pugbot_test
export PUGBOT_TEST_DATABASE_URL="postgres://pugtest@localhost/pugbot_test?host=/tmp/pgtest&port=55432"
```

## Operations

- `GET /health` — liveness. Does not touch the database, so a database outage
  does not cause a restart loop.
- `GET /ready` — readiness; returns 503 when the database is unreachable.
- `GET /metrics` — Prometheus counters, labelled with the mode.

Background jobs take a lease in `job_locks` keyed by `(name, mode)`, so two
processes never run the same sweep and debug never contends with production.
Audit rows and log lines all carry the mode.

Gateway intents are requested from configuration: the privileged
`GUILD_PRESENCES` intent is only requested when some channel actually has
presence-based queue removal switched on.

## Decisions

The specification left eight decisions open. These are the choices made:

1. **Serenity and PostgreSQL.** Queries use the runtime `sqlx` API rather than
   the compile-time macros, so `cargo build` and `cargo clippy` work without a
   live database; the SQL is covered by the integration tests instead.
2. **Slash commands only.** No message-prefix compatibility layer.
3. **Queue scope is configurable** per channel (`/channel set queue-scope:`).
   The default is `guild`: any live match in the server blocks queueing.
4. **Ready-check failure** cancels the match. Who returns to the queue is
   configurable (`ready_only`, `ready_and_pending`, `none`); a player who
   declined is never returned. **Match expiry** ends a match `EXPIRED` and
   unrated — with no evidence of the result, inventing one would corrupt
   ratings.
5. **Result consensus** requires one report from each team. Disagreement moves
   the match to `REPORT_PENDING` for a moderator; `/match report` overrides.
   Corrections are audited rather than silently overwriting.
6. **All three rating systems** are implemented; flat is the default.
7. **No web UI.** Configuration is entirely through slash commands.
8. **No GPL code was copied.** The behaviour was reimplemented from the
   specification; PUBobot2's source was not used.

## Documentation

| Document | For |
| --- | --- |
| [Player guide](docs/player-guide.md) | Playing a game |
| [Administrator guide](docs/administrator-guide.md) | Setting up and moderating a server |
| [Operations runbook](docs/operations.md) | Deploying, monitoring, and recovering |
| [Architecture](docs/architecture.md) | How it is built and why |
| [Data model](docs/data-model.md) | The schema and its constraints |
| [Traceability](docs/traceability.md) | Requirement → code → test |
| [Glossary](docs/glossary.md) | Terminology |
| API reference | `cargo doc --no-deps --open` |

[`docs/README.md`](docs/README.md) indexes the set and records the content model
it follows, along with the lints and tests that keep it honest.

## License

MIT or Apache-2.0, at your option.
