# Operations runbook

## Scope

Deploying, monitoring, and recovering a PUGbot deployment. For configuring a
server through Discord, see the [administrator guide](administrator-guide.md).

## Prerequisites

| Component | Requirement |
| --- | --- |
| Rust | 1.85 or later, to build |
| PostgreSQL | 14 or later — partial unique indexes and `make_interval` are used |
| Discord application | One per mode. **Never share one between debug and production** |
| Bot permissions | Send Messages, Embed Links, Use Application Commands, Read Message History |
| Gateway intents | Guilds, Guild Members. `GUILD_PRESENCES` only if any channel uses presence-based removal |

## First deployment

```bash
# 1. Build without debug tooling — the reset command must not exist in production
cargo build --release

# 2. Configure
cp .env.example .env.production      # fill in PUGBOT_PRODUCTION_*

# 3. Validate before connecting to anything
./target/release/pugbot --mode production --env-file .env.production check

# 4. Apply the schema
./target/release/pugbot --mode production --env-file .env.production migrate

# 5. Run
./target/release/pugbot --mode production --env-file .env.production
```

Step 3 is the important one. It loads configuration, runs the mode-isolation
cross-checks, and opens a database connection — everything except Discord. If it
passes, a startup failure afterwards is a Discord problem, not a configuration
problem.

### Reading the startup banner

```
starting PUGbot summary=mode=production application_id=123456789 \
  database=postgres://db.internal:5432/pugbot guilds=[<any>] owners=2 health=0.0.0.0:8080
```

Printed before anything connects. Check the application ID and database are the
ones you meant. Credentials are never included — the database URL is stripped of
both userinfo and query string.

## Configuration reference

Every variable takes the prefix of its mode: `PUGBOT_DEBUG_` or
`PUGBOT_PRODUCTION_`.

| Variable | Required | Default | Notes |
| --- | --- | --- | --- |
| `DISCORD_TOKEN` | Yes | — | Must differ between modes |
| `APPLICATION_ID` | Yes | — | Discord snowflake |
| `DATABASE_URL` | Yes | — | Must differ between modes |
| `DATABASE_MAX_CONNECTIONS` | No | `10` | |
| `GUILD_ALLOWLIST` | Debug only | — | Comma-separated. In production, empty means any guild |
| `OWNER_IDS` | No | empty | Comma-separated |
| `LOG_LEVEL` | No | `pugbot=debug,info` / `pugbot=info,warn` | `tracing` filter directive |
| `HEALTH_BIND` | No | disabled | e.g. `0.0.0.0:8080` |
| `PUBLIC_URL` | No | none | |

Startup **refuses** to continue if the two modes share a token, share a database
URL, or list the same guild; or if debug has no guild allowlist. There is no
fallback between modes: a missing debug value is an error, never a reason to
read the production one.

## Health and monitoring

| Endpoint | Meaning | Use for |
| --- | --- | --- |
| `GET /health` | Process is alive. Does **not** touch the database | Liveness probe |
| `GET /ready` | Database reachable; 503 if not | Readiness probe |
| `GET /metrics` | Prometheus counters, labelled with the mode | Scraping |

Liveness deliberately ignores the database. A database outage should take the
bot out of rotation, not into a restart loop that cannot fix anything.

### Metrics

| Metric | Meaning |
| --- | --- |
| `pugbot_queue_joins_total` | Successful queue joins |
| `pugbot_queue_expiries_total` | Queue slots swept for expiry |
| `pugbot_matches_started_total` | Matches launched |
| `pugbot_matches_completed_total` | Matches finalised with a result |
| `pugbot_command_failures_total` | Commands that failed with an internal error |

### Suggested alerts

| Alert | Condition | Means |
| --- | --- | --- |
| Bot down | `/health` failing for 2 min | Process dead or wedged |
| Database unreachable | `/ready` 503 for 5 min | Database or network problem |
| Timers stalled | `queue_expiries_total` flat while matches are live for 10 min | Job loop stopped or a lease is stuck |
| Error rate | `command_failures_total` rising | Check logs for the underlying error |

### Logs

Production emits JSON. Every line carries `mode`. Command spans carry the
command name, guild, channel, and actor.

```bash
# Errors only
journalctl -u pugbot -o cat | jq 'select(.level=="ERROR")'

# One guild
journalctl -u pugbot -o cat | jq 'select(.span.guild==123456789)'
```

Secrets are never logged: tokens and database URLs are wrapped in a type whose
`Debug` and `Display` both render `[redacted]`.

## Routine operations

### Deploying a new version

Migrations are idempotent and are applied automatically at startup, so a rolling
restart is safe.

```bash
cargo build --release
sudo systemctl restart pugbot
curl -fsS localhost:8080/ready
```

On startup the bot logs how many live matches it resumed and immediately runs
one timer sweep, so anything that timed out during the restart is resolved at
once.

### Backups

```bash
pg_dump --format=custom --file=pugbot-$(date +%F).dump "$PUGBOT_PRODUCTION_DATABASE_URL"
```

Everything that matters is in the database; the process holds no durable state.

**Test the restore.** A backup that has never been restored is a hypothesis:

```bash
createdb pugbot_restore_test
pg_restore --dbname=pugbot_restore_test pugbot-2026-08-17.dump
psql pugbot_restore_test -c 'SELECT count(*) FROM matches;'
dropdb pugbot_restore_test
```

### Rotating the bot token

1. Generate a new token in the Discord developer portal.
2. Update `PUGBOT_PRODUCTION_DISCORD_TOKEN`.
3. `pugbot --mode production check`.
4. Restart.

Live matches are unaffected — they are in the database, not in memory.

## Incident response

### The bot is not responding to commands

1. `curl localhost:8080/health` — if it fails, the process is dead. Restart.
2. `curl localhost:8080/ready` — 503 means the database is unreachable. Fix that
   first; the bot recovers on its own.
3. Check the logs for gateway disconnects. Serenity reconnects automatically.
4. If commands are missing rather than failing, they may not be registered.
   Global commands can take up to an hour to propagate.

### A match is stuck

Matches advance on player action or on the timer sweep, so a stuck match usually
means the timer loop is not running.

```sql
-- What is live, and what is it waiting for?
SELECT match_id, state, check_in_ends_at, vote_ends_at, expires_at
FROM matches
WHERE state NOT IN ('COMPLETED','CANCELLED','EXPIRED')
ORDER BY created_at;

-- Is a job lease stuck?
SELECT * FROM job_locks;
```

A lease is held for 60 seconds. If `locked_until` is far in the past and nothing
is progressing, the holder died; the next tick takes it over. If it is in the
future and held by a dead process, delete the row.

As a last resort, a moderator can `/match cancel` the match.

### Ratings look wrong

Every change is recorded:

```sql
SELECT created_at, rating_before, rating_after, reason, actor_id
FROM rating_history
WHERE channel_id = $1 AND user_id = $2
ORDER BY created_at DESC LIMIT 20;
```

`match_id` identifies a match-derived change; `reason` and `actor_id` identify a
manual one.

A match cannot be rated twice: the `rated` flag is claimed inside the finalising
transaction, and a unique index on `(match_id, user_id)` in `rating_history`
backs it up. If a rating looks doubled, it was two separate events — the history
shows both.

### Recovering from a bad configuration change

Configuration changes are audited with before and after values:

```sql
SELECT created_at, actor_id, action, data
FROM audit_events
WHERE guild_id = $1 AND action LIKE '%configured'
ORDER BY created_at DESC LIMIT 10;
```

`data->'before'` holds the previous settings; re-apply them with the matching
`/queue set-*` or `/channel set` command.

## Debug deployments

Debug mode is for development and integration testing, not a staging copy of
production.

```bash
cargo build --features debug-tools
./target/debug/pugbot --mode debug --env-file .env.debug
```

| Guarantee | Mechanism |
| --- | --- |
| Cannot use production credentials | Reads only `PUGBOT_DEBUG_*`; refuses a shared token or database |
| Cannot act on production guilds | Allowlist required and enforced on every interaction; a guild in both allowlists is refused at startup |
| Cannot contend with production jobs | Job leases keyed by `(name, mode)` |
| Debug data is identifiable | `matches.mode` and `audit_events.mode` |

Resetting a debug database:

```bash
pugbot --mode debug --env-file .env.debug debug-reset --yes-delete-everything
```

Guarded three times over: the `debug-tools` feature, the runtime mode check, and
the acknowledgement flag. A production build does not contain the subcommand,
which CI asserts by inspecting the release binary.

## Capacity

Rough figures for a single process; nothing here has been load-tested.

| Resource | Note |
| --- | --- |
| Database connections | Default pool of 10. Each command uses one briefly; the timer loop uses one every 15 s |
| Memory | No durable state in memory. Usage tracks the Serenity cache, which scales with guild and member count |
| Timer resolution | 15 s. A check-in configured shorter than that still resolves promptly, because a ready press advances the match directly |

Two replicas would coexist safely — job leases and optimistic locking are built
for it — but this has not been tested.

## Security

| Control | Implementation |
| --- | --- |
| Secrets never logged | `Secret` type renders `[redacted]` in both `Debug` and `Display` |
| Database URLs redacted | Userinfo and query string stripped before logging |
| Least-privilege intents | `GUILD_PRESENCES` requested only when configuration uses it |
| Mass-mention protection | `@everyone` and `@here` suppressed in all bot output |
| Internal errors not leaked | Only user errors are shown verbatim; anything else becomes a generic message and is logged |
| Destructive commands gated | Permission checks, plus audit rows for every one |

Recommended database grants: `SELECT`, `INSERT`, `UPDATE`, `DELETE` on the
application tables. `CREATE` is needed only when migrations run — if you apply
migrations separately, the runtime role does not need it.

## Pre-production checklist

- [ ] Release binary built **without** `debug-tools`
- [ ] `pugbot --mode production check` passes
- [ ] Debug and production use different Discord applications
- [ ] Debug and production use different databases
- [ ] No guild appears in both allowlists
- [ ] Health endpoint bound and probed
- [ ] Metrics scraped and alerts configured
- [ ] Backups scheduled **and a restore tested**
- [ ] Log aggregation receiving JSON
- [ ] `GUILD_PRESENCES` granted only if actually used
- [ ] Database role holds no more privilege than it needs
