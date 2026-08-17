# PUGbot Feature and Implementation Plan

## Purpose

PUGbot is a Discord bot for organizing pickup games (PUGs). Each PUG-enabled Discord channel owns exactly one queue. The bot should take players from that channel's queue through check-in, team formation, map selection, match reporting, rating updates, and historical statistics. Server staff should be able to configure each channel queue independently.

This plan is based on the behavior of [Leshaka/PUBobot2](https://github.com/Leshaka/PUBobot2), reviewed on 17 August 2026. It is an implementation specification, not a request to copy the reference code. PUBobot2 is licensed under GPL-3.0; reusing or adapting its code requires compliance with that license.

## Product goals

- Make joining and leaving a PUG take one command or interaction.
- Automatically start a match when a queue fills.
- Make check-in, drafting, map selection, substitutions, and reporting clear in Discord.
- Support casual and ranked queues in the same server.
- Give moderators strong recovery and correction tools.
- Persist configuration, ratings, match history, and active state safely across restarts.
- Support multiple guilds and channels, one queue per enabled channel, and multiple languages.
- Run in a clearly separated debug mode for development and Discord integration testing without touching production bot credentials, guilds, or data.
- Implement the bot in Rust for strong type safety, predictable performance, and reliable asynchronous concurrency.

## Roles and permissions

| Role | Capabilities |
| --- | --- |
| Player | Join/leave queues, check in, draft when captain, request/substitute, report results, view stats, manage personal preferences |
| Moderator | Manage queue membership, start the channel queue, alter active matches, report/cancel results, issue queue bans, manage player phrases |
| Administrator | Enable channels, create/configure/delete queues, configure ratings and rank roles, reset or migrate statistics |
| Bot owner | Global diagnostics, guild disable/enable, lifecycle and maintenance operations |

Discord permission checks and configured admin/moderator roles should both be supported. Every destructive moderation command should require confirmation and create an audit log entry.

## Core user journey

```text
Player joins queue
       |
Queue reaches configured size (or moderator starts it)
       |
Optional check-in ---- timeout/decline ---> return players to queue or abort
       |
Team formation: captain draft / rating-balanced / random / no teams
       |
Optional map selection or vote
       |
Match announced, DMs sent, server details shown
       |
Substitution if required
       |
Result consensus or moderator report
       |
Match history + ratings + ranks + leaderboards updated
```

## Feature requirements

### 1. Multi-guild, channel, and queue configuration

- Enable or disable PUGbot per text channel.
- Create at most one queue for each enabled channel. Enforce this with a database uniqueness constraint on the channel ID.
- Treat the Discord channel as the queue boundary: queue commands issued in a channel always target that channel's single queue and do not accept queue names or aliases.
- Configure queue size, automatic start, ranked/unranked behavior, team size, check-in, match lifetime, maps, server text, and appearance independently.
- Configure channel-wide command behavior, language, roles, auto-removal, ratings, ranks, and leaderboard rules.
- Offer slash commands as the primary interface. Legacy prefix/message commands are optional compatibility work.

### 2. Queue lifecycle

- `/add`: join the current channel's queue.
- `/remove`: leave the current channel's queue.
- `/who`: display queued players and remaining slots in the current channel.
- `/promote`: announce the current channel's queue and mention its subscription/promotion role, subject to cooldown.
- `/subscribe` and `/unsubscribe`: self-assign/remove promotion roles.
- `/server`, `/maps`, and `/map`: show server information, map pool, or a random map.
- Automatically start a full queue when `autostart` is enabled.
- Allow moderators to add/remove players, clear, or manually start the channel queue. Splitting an oversized queue is out of scope because a channel represents one queue and one match launch at a time.
- Prevent duplicate membership, enforce queue/channel whitelist and blacklist roles, and enforce active timed queue bans.
- Display an optional randomized player-specific phrase when that player joins.

### 3. Queue expiry and presence handling

- Optionally remove queued players who become AFK or offline.
- Let a player temporarily opt out of offline removal where allowed.
- Support a channel default expiry timer, a per-player default, and a current-session override.
- Expiry is refreshed by joining and capped by a safe channel maximum.
- Allow one-use automatic readiness with a configurable duration and channel maximum.
- Periodic cleanup must be idempotent and safe after restarts.

### 4. Check-in

- Start an optional ready-check when the queue launches.
- Support Ready and Not Ready through slash commands and message components/buttons.
- Show ready, pending, and declined players plus time remaining.
- Optionally abort immediately on a decline or wait until timeout.
- Define a consistent policy for returning ready/pending players to the queue when check-in fails.
- Honor an unexpired one-use auto-ready preference.

### 5. Team formation and captains

Support four formation modes:

- **Captain draft:** captains pick from the unassigned player pool.
- **Rating matchmaking:** choose teams with the smallest practical rating difference.
- **Random teams:** shuffle players into configured team sizes.
- **No teams:** announce a player list only.

Captain selection modes:

- By captain role preference and rating.
- A close-rating (fair) captain pair.
- Random with captain-role preference.
- Fully random.
- No automatic captains; players volunteer with `/capfor`.

Draft behavior:

- Configurable pick order such as `ABABABBA`.
- `/pick <player>` for the active captain only.
- `/capme` to vacate a captain position and `/capfor <team>` to claim one.
- Reject duplicate/invalid picks and advance atomically.
- Show team names, emojis, captains, members, ratings, and unpicked players in an updated embed.

### 6. Maps

- Maintain a map pool per queue.
- Randomly select a configured number of maps.
- Avoid recently played maps according to a configurable cooldown where the pool permits it.
- Optionally present 2–9 candidate maps as a vote during check-in.
- Resolve ties deterministically or randomly and record both candidates and the selected map(s).

### 7. Match operation

- Announce match ID, teams/players, selected maps, queue, server details, and optional custom start message.
- Optionally list players who are streaming.
- Send configurable start DMs, respecting each player's DM preference and Discord failures.
- `/teams` and `/matches` show the caller's match or active channel matches.
- `/subme` requests replacement and optionally mentions a promotion role.
- `/subfor <player>` lets an eligible player replace the requester.
- Moderators can force a substitution or move a player among teams and the unpicked pool.
- Ranked matches expire after a configurable lifetime; expiry must have an explicit cancel/no-rating policy.
- Persist active queue/match state frequently enough to recover after process restarts.

### 8. Result reporting

- Players report a team win, draw, or cancellation.
- Require consensus from the opposing side/captains before finalizing, or permit moderator override.
- Record scores as well as winner/draw/cancel status.
- Ensure finalization is transactional and idempotent so a match cannot update ratings twice.
- Moderators may report an active match or create a historical/manual ranked result.
- Preserve corrections in an audit trail instead of silently overwriting history.

### 9. Ratings, ranks, and leaderboards

Support selectable per-channel rating systems:

- Flat rating changes.
- Glicko-2.
- TrueSkill.

Rating configuration should include initial rating/deviation, minimum deviation, overall/win/loss scaling, draw bonus, optional winning/losing streak multipliers, inactivity rating decay, and deviation decay.

Ranked results update wins, losses, draws, streak, last-ranked-match time, rating, deviation, and immutable rating history. Rank thresholds can optionally assign Discord roles and prefix nicknames. A channel may share another channel's rating pool.

Player/stat commands:

- `/rank [player]`: rating, deviation/uncertainty, rank, W/L/D, streak, and recent change.
- `/leaderboard [page]`: paginated ranking with configurable minimum matches and recent-activity cutoff.
- `/top`: most active players.
- `/lastgame [queue|player]`: latest matching result.
- `/stats show`: channel totals and activity.
- `/nick`: apply the configured rating/rank nickname prefix.

Admin rating tools should seed rating/deviation, apply a penalty with reason, hide/unhide a player, reset a channel, snap ratings to rank floors, reset one player, and merge/replace player records. All adjustments must write rating history.

### 10. Moderation

- Guild-wide timed queue bans (“no-adds”) with reason, issuer, start, duration, expiry, and releaser.
- List active bans and allow early release.
- Queue/channel blacklist and whitelist roles.
- Add or clear custom join phrases for a player.
- Audit configuration changes, manual match operations, bans, stat resets, and rating adjustments.

### 11. Personal settings and notifications

- Toggle match-start DMs.
- Set the player's default queue expiry.
- Override expiry for the current queued session.
- Temporarily allow offline queue retention.
- Arm auto-ready for the next match.
- Subscribe to channel- or queue-specific promotion roles.

### 12. Localization and help

- Externalize every user-facing string.
- Resolve language per enabled channel, with English fallback.
- Initial parity target: English, French, Russian, Spanish, Italian, Korean, Brazilian Portuguese, and Turkish.
- `/help` shows the current channel queue's description and relevant workflows.
- `/commands` links to or displays the current command reference.

## Recommended command surface

| Area | Player commands | Staff commands |
| --- | --- | --- |
| Queues | `add`, `remove`, `who`, `promote`, `subscribe`, `unsubscribe`, `server`, `maps`, `map` | `queue create`, `show`, `set`, `delete`, `add-player`, `remove-player`, `clear`, `start` |
| Match | `ready`, `notready`, `teams`, `matches`, `capfor`, `capme`, `pick`, `subme`, `subfor`, `report` | `match report`, `create`, `sub-player`, `put`, `cancel` |
| Stats | `rank`, `leaderboard`, `top`, `lastgame`, `stats show`, `nick` | `rating seed`, `penalty`, `hide`, `unhide`, `reset`, `snap`; `stats reset`, `reset-player`, `replace-player` |
| Preferences | `switch-dms`, `expire`, `expire-default`, `auto-ready`, `allow-offline` | — |
| Moderation | — | `noadds list`, `add`, `remove`; `phrases add`, `clear` |
| Configuration | `help`, `commands` | `channel enable`, `disable`, `show`, `set` |

Use Discord-native autocomplete, choices, buttons, confirmations, ephemeral error messages, and paginated embeds. Command names can use underscores instead of hyphens if that matches the chosen Discord library conventions.

## Suggested architecture

PUGbot must be implemented in Rust. A suitable baseline stack is:

- Tokio for the asynchronous runtime and background jobs.
- Serenity or Twilight for Discord gateway, interactions, components, and HTTP APIs; select one before implementation and keep it behind the Discord adapter boundary.
- SQLx for compile-time-checked SQL, transactions, connection pooling, and migrations.
- Serde for configuration and persisted JSON snapshots.
- Tracing and tracing-subscriber for structured, mode-aware logs.
- Thiserror for domain/application errors and Anyhow only at executable boundaries.
- Clap for explicit command-line mode selection and operational commands.
- Rustls-based TLS where supported to avoid an OpenSSL runtime dependency.

```text
Discord adapters (slash commands, buttons, events, embeds)
                         |
Application services (queue, check-in, draft, match, rating, moderation)
                         |
Domain model + explicit state machines + permission policies
                         |
Repositories / unit of work / scheduler / event and audit log
                         |
PostgreSQL or MySQL                  Optional Redis
```

Keep Discord objects out of the core domain where practical. Use stable guild, channel, role, and user IDs at service boundaries. Model queue and match transitions explicitly rather than scattering state changes among command handlers.

Suggested modules:

```text
src/
  main.rs        # process startup and explicit mode selection
  discord/       # commands, components, embeds, event listeners
  domain/        # queue, match, draft, rating types and invariants
  services/      # use cases and transaction boundaries
  repositories/  # persistence traits and SQLx implementations
  jobs/          # expiry, ready-check, decay, role-sync workers
  localization/  # catalogs and locale resolution
  config/        # environment and validated guild/queue configuration
  observability/ # logging, metrics, health, audit events
tests/
  unit/
  integration/
  e2e/
```

Use Rust enums for queue/match states and report outcomes, newtypes for Discord/entity IDs, and traits for Discord, clock, rating, and repository boundaries. Avoid holding a database transaction across Discord network calls.

## Match state model

Recommended states are `QUEUED`, `CHECK_IN`, `TEAM_FORMATION`, `MAP_VOTE`, `ACTIVE`, `REPORT_PENDING`, `COMPLETED`, `CANCELLED`, and `EXPIRED`. Persist every transition with a version number. Commands should state which source states they accept, and concurrent transitions should use optimistic locking or row locks.

Important invariants:

- A user appears at most once in a queue and at most once in a match roster.
- A queued player cannot simultaneously be active in another match in the same configured scope.
- A player belongs to at most one team in a match.
- Only the active captain can make the current draft pick.
- A completed/cancelled match cannot transition again without an audited administrative correction workflow.
- Rating updates are generated once per finalized ranked match.

## Data model

Minimum persistent entities:

- `guilds`: enabled state and global moderation metadata.
- `channel_configs`: Discord channel, roles, locale, presence/expiry rules, rating and leaderboard settings.
- `queues`: channel ID, size, access roles, and start/check-in/team/map/appearance settings; `channel_id` is unique so a channel cannot own more than one queue.
- `queue_members`: queue, user, joined time, expiry, and status; unique `(queue_id, user_id)`.
- `users`: DM and default expiry preferences.
- `channel_players`: rating, deviation, W/L/D, streak, hidden flag, last ranked match.
- `matches`: channel/queue, state, version, timestamps, configuration snapshot, winner/scores, maps.
- `match_players`: user, team, captain flag, ready status, substitution links, rating snapshot.
- `draft_picks`: match, sequence, captain/team, selected player, timestamp.
- `map_votes`: candidates and one vote per eligible user.
- `rating_history`: before/change values, match or adjustment reason, actor, timestamp.
- `queue_bans`: guild/user, issuer, reason, start, duration, release information.
- `player_phrases`: channel/user/phrase.
- `subscriptions`: user and promotion role/queue.
- `audit_events`: actor, action, target, structured before/after data, timestamp.

Prefer schema migrations over runtime table mutation. Store the effective match configuration as a JSON snapshot so later queue changes do not alter historical interpretation.

## Configuration and secrets

Runtime configuration should come from environment variables or a secret manager:

- Discord bot token, application/client ID, and owner IDs.
- Database URL and connection-pool limits.
- Log level and deployment environment.
- Optional public URL, OAuth redirect URL, and web/health server binding.
- Optional error reporting and metrics endpoints.

Never commit tokens or database passwords. Validate required settings at startup and fail with a clear message. Request only the Discord intents and permissions used by enabled features; presence-based removal and member/role synchronization require the relevant privileged intents.

## Separated debug mode

Debug mode is an explicit application mode for local development and integration testing, not merely a more verbose log level. Start it with an unambiguous argument such as `pugbot --mode debug`; production should require `--mode production`. Do not silently default to production.

Debug and production must be isolated as follows:

| Concern | Debug mode | Production mode |
| --- | --- | --- |
| Discord application | Dedicated test bot token/application | Production bot token/application |
| Discord scope | Allowlisted test guild IDs only; register guild-scoped commands for fast updates | Approved production guilds; global commands where required |
| Database | Dedicated debug database or schema | Production database/schema |
| Cache/queues | Dedicated namespace | Production namespace |
| Logs | Verbose structured logs with source locations and backtraces | Operational structured logs; secrets and personal data redacted |
| External notifications | Disabled or routed to test destinations | Production destinations |
| Destructive commands | Permitted only inside allowlisted test guilds, still confirmed and audited | Normal production authorization and confirmation rules |

Required safeguards:

- Use separate `PUGBOT_DEBUG_*` and `PUGBOT_PRODUCTION_*` configuration sources, or separate environment files/secret paths selected by mode.
- Never fall back from missing debug credentials to production credentials, or vice versa.
- On startup, print the selected mode, Discord application ID, database host/name (without credentials), and allowed guild IDs; require all values to pass cross-checks before connecting.
- Reject every debug-mode interaction originating outside the debug guild allowlist.
- Include `mode=debug|production` in logs, metrics, audit events, health responses, and background-job locks.
- Give debug data an obvious marker and provide a debug-only reset command that cannot compile or execute against production configuration.
- Support fake clock, in-memory Discord adapter, and repository test doubles for deterministic unit tests; use the real debug bot and database only for integration/end-to-end tests.
- Keep behavioral logic identical between modes. Debug-only shortcuts belong in adapters or test tooling and must not change domain rules.
- CI should use test configuration and ephemeral databases, never either long-lived debug or production secrets.

## Reliability, security, and operations

- Use database transactions for match completion, substitutions, queue starts, resets, and record merges.
- Add idempotency guards to Discord interactions and scheduled jobs.
- Recover active queues, check-ins, drafts, votes, and matches on startup.
- Use structured logs carrying guild, channel, queue, match, interaction, actor, and correlation IDs.
- Provide health/readiness endpoints and metrics for queue joins, match starts/completions, command failures, job lag, and Discord/database latency.
- Rate-limit promotions and costly admin/stat operations.
- Escape mentions and sanitize configurable text; use Discord allowed-mentions controls.
- Apply least-privilege database and Discord credentials.
- Handle missing members, deleted roles/channels, forbidden DMs, Discord rate limits, and partial outages without losing match state.
- Back up the database and test restoration before production use.

## Testing strategy

- Unit-test queue eligibility, expiration, captain selection, every draft order, balanced/random teams, map cooldown/voting, report consensus, and each rating adapter.
- Property-test invariants: no duplicated players, complete team partitioning, valid draft termination, and rating finalization exactly once.
- Integration-test database constraints, migrations, concurrent joins, simultaneous final reports, restart recovery, scheduled expiry, and role synchronization.
- End-to-end test the main Discord flows with a test guild or mocked interaction adapter.
- Snapshot-test key embeds while keeping domain assertions independent of presentation.
- Add migration tests using a production-like database version.
- Test mode isolation: debug startup must fail with production credentials, non-allowlisted guild interactions must be rejected, and debug reset tooling must be unavailable in production builds/configuration.
- Run standard Rust quality gates: `cargo fmt --check`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test --all-features`, and dependency/security auditing in CI.

## Delivery phases

### Phase 1 — Foundation and casual queues

- Rust workspace/project skeleton, Tokio runtime, validated configuration, SQLx migrations, structured tracing, and Discord connection.
- Explicit debug/production modes with separate bot credentials, guild allowlist, database, logs, and startup safety checks.
- Channel enablement and single-queue-per-channel CRUD, including uniqueness enforcement.
- Join, leave, list, promote, access-role checks, and manual/automatic start.
- Random/no-team formation, match announcement, cancellation, and persistence.

**Exit:** two guilds can independently configure queues and complete unranked matches through slash commands.

### Phase 2 — Check-in, drafts, maps, and recovery

- Ready-check and timers.
- Captain selection, draft state machine, balanced teams, team display.
- Map pools, cooldowns, map voting, start DMs, and substitutions.
- Active-state restart recovery and moderator repair commands.

**Exit:** a process restart during check-in/draft/active play resumes without duplicated or lost state.

### Phase 3 — Ranked play and statistics

- Match report consensus and transactional completion.
- Flat, Glicko-2, and TrueSkill adapters.
- Player profiles, match history, leaderboards, ranks, nickname/role sync, decay, and rating history.
- Rating and statistics administration.

**Exit:** repeated or concurrent result submissions cannot produce duplicate history or rating changes.

### Phase 4 — Moderation, localization, and production hardening

- Timed queue bans, player phrases, personal preferences, and audit views.
- Translation catalogs and locale fallback.
- Observability, health checks, rate limits, load/concurrency tests, backups, and deployment documentation.

**Exit:** production runbook, tested restore procedure, monitoring, and permissions review are complete.

## Definition of done

A feature is complete when its permission rules and state transitions are explicit; persistent changes are transactional and audited where appropriate; user errors are actionable; Discord/API failures are handled; restart behavior is defined; unit and integration coverage includes success, rejection, concurrency, and timeout paths; strings are localizable; and command/help documentation is updated.

## Decisions to make before implementation

1. Select Serenity or Twilight as the Rust Discord adapter, and select PostgreSQL or MySQL for SQLx. Rust is the required implementation language.
2. Decide whether prefix/message commands are required or slash commands are sufficient.
3. Define whether a player can join queues in multiple channels while already queued or in a match elsewhere.
4. Define the exact ready-check failure and match-expiry policies.
5. Define who must confirm results and how disputes/corrections work.
6. Choose the initial rating system and whether all three reference algorithms are needed for v1.
7. Decide whether a web configuration UI is in scope; the reference README mentions one, but it is not present in the reviewed repository.
8. Confirm GPL strategy before copying any source code rather than reimplementing behavior.

## Reference parity checklist

- [ ] Exactly one configurable queue per enabled channel, enforced by the database and service layer
- [ ] Channel queue promotion subscriptions
- [ ] Auto-start, manual start, and membership moderation
- [ ] Presence and timer-based removal, auto-ready, DM preferences
- [ ] Ready-check with timeout/decline policies
- [ ] Draft, matchmaking, random, and no-team modes
- [ ] Configurable captains, pick order, names, and emojis
- [ ] Map pool, recent-map avoidance, and map voting
- [ ] Match announcements, server text, streams, DMs, and substitutions
- [ ] Win/draw/cancel reports and moderator controls
- [ ] Flat, Glicko-2, and TrueSkill ratings with full history
- [ ] Rank roles/nicknames, decay, leaderboards, profiles, and stats
- [ ] Timed queue bans and player join phrases
- [ ] Channel/queue access roles and staff permission levels
- [ ] Localization, help, audit logs, persistence, and restart recovery
