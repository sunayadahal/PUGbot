# Administrator and moderator guide

## Scope

Setting up, configuring, and moderating PUGbot on a Discord server. For playing,
see the [player guide](player-guide.md). For running the process itself, see
[operations](operations.md).

## Permission levels

| Level | Granted by | Can |
| --- | --- | --- |
| Player | Everyone | Queue, play, report, manage own settings |
| Moderator | Discord *Manage Messages*, **or** the configured moderator role | Manage queue membership and live matches, issue queue bans, manage join phrases |
| Administrator | Discord *Manage Guild* or *Administrator*, **or** the configured admin role | Everything a moderator can, plus channel and queue configuration, ratings and statistics administration |
| Owner | Listed in the bot's `OWNER_IDS` | Everything, in every guild |

Native Discord permissions and configured roles both grant a level, and the
higher of the two wins. You do not need to configure roles at all if your
Discord permissions already reflect who should do what.

---

## Setting up a channel

### 1. Enable the channel

```
/channel enable
```

One channel owns exactly one queue. That is enforced by the database, not just
by convention — a second `/queue create` in the same channel is refused, even if
two administrators run it simultaneously.

### 2. Create the queue

```
/queue create name:pug size:10 teams:2 ranked:true
```

| Option | Meaning | Default |
| --- | --- | --- |
| `name` | Display name | `pug` |
| `description` | Shown by `/help` | none |
| `size` | Players needed to launch | `10` |
| `teams` | How many teams | `2` |
| `autostart` | Launch automatically when full | `true` |
| `ranked` | Results move ratings | `false` |
| `match-lifetime` | Seconds before an unreported match expires | `10800` |
| `server` | Connection details shown at match start | none |
| `start-message` | Extra text at match start | none |
| `start-dm` | Send match-start DMs | `true` |
| `show-streams` | List streaming players | `false` |

`size` must divide evenly into `teams` unless team formation is `no_teams`. An
invalid combination is refused when you set it, not discovered mid-match.

### 3. Configure the rest

Queue settings are edited through four themed subcommands. Discord allows at
most 25 options per subcommand and the full configuration has more than that, so
one giant command is not possible — and would be unusable if it were.

```
/queue set-basics    size, teams, ranked, autostart, match text
/queue set-teams     team formation, captains, pick order, check-in
/queue set-maps      map pool, cooldown, voting
/queue set-roles     access, promotion, and captain roles
```

Between them these cover every setting; a test enforces that.

To review the current configuration:

```
/queue show
/channel show
```

---

## Team formation

`/queue set-teams team-formation:`

| Mode | Behaviour |
| --- | --- |
| `captain_draft` | Captains take turns picking |
| `rating_matchmaking` | Split by smallest rating difference |
| `random_teams` | Shuffle into equal teams |
| `no_teams` | Announce a player list only |

**Balance quality:** for two teams and up to 20 players, the split is optimal —
every possible balanced split is searched. Larger rosters and three-or-more-team
splits use a greedy pass refined by pairwise swaps: good, but not guaranteed
optimal.

### Captains

`/queue set-teams captains:`

| Mode | Behaviour |
| --- | --- |
| `role_and_rating` | Captain-role holders first, then highest rating |
| `fair_pair` | The pair with the closest ratings |
| `random_with_role_preference` | Random, but role holders are drawn first |
| `random` | Uniformly random |
| `volunteer` | Nobody appointed; players claim slots with `/capfor` |

### Pick order

```
/queue set-teams pick-order:ABBA
```

Letters are team numbers: `A` is team 1. The pattern repeats if more picks are
needed than it describes. `ABBA` gives the second captain two picks in a row,
which many communities consider fairer than strict alternation.

The order is a preference, not a constraint: if the pattern only ever names a
full team, the turn passes to a team with room so the draft always finishes.

---

## Check-in

```
/queue set-teams check-in:180 check-in-abort-on-decline:true check-in-return:ready_and_pending
```

| Option | Meaning |
| --- | --- |
| `check-in` | Seconds to answer. **`0` disables check-in entirely.** |
| `check-in-abort-on-decline` | Abort as soon as somebody declines, rather than waiting out the timer |
| `check-in-return` | Who goes back into the queue on failure |

`check-in-return` options:

| Value | Returned to the queue |
| --- | --- |
| `ready_only` | Only players who pressed Ready |
| `ready_and_pending` | Ready and silent players |
| `none` | Nobody |

**A player who pressed Not ready is never returned, under any policy.** They
said no.

---

## Maps

```
/queue set-maps maps:de_dust2,de_inferno,de_mirage,de_nuke map-count:1 map-cooldown:3
```

| Option | Meaning |
| --- | --- |
| `maps` | Comma-separated pool |
| `map-count` | How many maps a match plays |
| `map-cooldown` | Avoid maps used in this many recent matches |
| `map-vote` | Candidates put to a vote, 2–9. **`0` disables voting.** |
| `map-vote-tie-break` | `deterministic` (first candidate) or `random` |

The cooldown relaxes automatically rather than deadlocking: if honouring it in
full would leave too few maps, the oldest restriction is released until enough
are available. A three-map pool with a three-match cooldown still works.

---

## Access control

```
/queue set-roles whitelist-role:@Member blacklist-role:@Muted promotion-role:@PUG captain-role:@Captain
```

| Role | Effect |
| --- | --- |
| `whitelist-role` | Required to join |
| `blacklist-role` | Blocks joining — **checked first, so a blacklist beats a whitelist** |
| `promotion-role` | Pinged by `/promote` and `/subme`; the role `/subscribe` manages |
| `captain-role` | Preferred when picking captains |

To clear roles:

```
/queue set-roles clear-roles:true
```

Discord options cannot express "set this to nothing", so this switch clears every
role field at once.

---

## Channel configuration

```
/channel set locale:fr admin-role:@Admin moderator-role:@Mod
```

### Language

```
/channel set locale:pt-BR
```

Available: `en`, `fr`, `ru`, `es`, `it`, `ko`, `pt-BR`, `tr`. An uninstalled
locale is refused with the list of valid ones.

### Presence-based removal

```
/channel set remove-offline:true remove-afk:true allow-offline-opt-out:true
```

**This requires the privileged `GUILD_PRESENCES` intent** to be enabled for the
bot application in the Discord developer portal. Without it Discord never sends
the event, and the feature is silently inert. The bot only requests the intent
when some channel has one of these switched on.

`allow-offline-opt-out` controls whether players may use `/allow-offline` to
stay queued while away.

### Expiry limits

```
/channel set default-expiry:14400 max-expiry:43200 max-auto-ready:3600
```

`max-expiry` caps whatever a player requests. `max-auto-ready:0` disables
auto-ready in the channel.

### Queue scope

```
/channel set queue-scope:guild
```

| Value | A player is blocked from queueing when… |
| --- | --- |
| `guild` (default) | They are in any live match in the server |
| `channel` | They are in a live match in this channel |

---

## Ratings

### Choosing a system

```
/channel set rating-system:glicko2
```

| System | Best for |
| --- | --- |
| `flat` (default) | Communities that want a simple, explainable number |
| `glicko2` | Ladders that need confidence tracking and handle irregular play |
| `trueskill` | Team games where individual contribution is uncertain |

### Tuning

```
/channel set initial-rating:1500 initial-deviation:200 min-deviation:50 \
             rating-scale:25 win-scale:1.0 loss-scale:1.0 draw-bonus:0
```

`rating-scale` is the base change per result under `flat`. `win-scale` and
`loss-scale` multiply it, so asymmetric ladders — where a loss costs less than a
win earns — are a configuration change, not a code change.

### Decay

```
/channel set decay-per-day:1.5 deviation-decay-per-day:2.0
```

Ratings decay **toward** the initial rating and never below it, so an inactive
strong player never sinks under a newcomer. Deviation grows back, so the system
becomes appropriately unsure about somebody who stopped playing.

### Ranks

Rank tiers assign names, emoji, and optionally Discord roles by rating floor.
After retuning thresholds:

```
/rating snap
```

lifts every rating to the floor of the tier it holds, so nobody sits
fractionally below the tier they were shown.

### Shared rating pools

```
/channel set rating-pool:#other-channel
```

Two channels then feed one ladder — useful for a 5v5 and a 2v2 channel that
should share a ranking.

### Rating administration

| Command | Effect | Audited |
| --- | --- | --- |
| `/rating seed player:@x rating:1600` | Set a rating outright | Yes |
| `/rating penalty player:@x amount:-50 reason:...` | Adjust with a stated reason | Yes |
| `/rating hide player:@x` | Remove from the leaderboard | Yes |
| `/rating unhide player:@x` | Restore | Yes |
| `/rating snap` | Snap every rating to its rank floor | Yes |

Every adjustment writes a `rating_history` row, so a rating can always be
explained.

---

## Moderation

### Queue membership

| Command | Effect |
| --- | --- |
| `/queue add-player player:@x` | Add somebody. Bypasses role checks, but **not** bans or the one-live-match rule |
| `/queue remove-player player:@x` | Remove somebody |
| `/queue clear` | Empty the queue |
| `/queue start` | Start with whoever is queued |

`/queue start` on a partly full queue trims the roster to a size the configured
teams divide evenly.

### Live matches

| Command | Effect |
| --- | --- |
| `/match report match:42 result:1` | Force a result |
| `/match cancel match:42` | Cancel — nothing is rated |
| `/match sub-player match:42 out:@x into:@y` | Swap a player |
| `/match put match:42 player:@x team:2` | Move between teams; omit `team` to return them to the unpicked pool |
| `/match create team1:... team2:... result:1` | Record a match played outside the bot |

Team numbers are 1-based, matching what players see.

### Queue bans

```
/noadds add player:@x seconds:86400 reason:no-shows
/noadds list
/noadds remove player:@x
```

Bans are guild-wide and timed. Issuing one also drops the player from every
queue they are sitting in. Bans are capped at one year — a typo cannot ban
somebody for a century — and are *released* rather than deleted, so who lifted
one and when stays on the record.

### Join phrases

```
/phrases add player:@x phrase:the legend returns
/phrases clear player:@x
```

One is chosen at random and shown when that player joins.

---

## Statistics

| Command | Effect |
| --- | --- |
| `/stats show` | Channel totals |
| `/stats reset` | **Deletes every rating in the channel.** Irreversible |
| `/stats reset-player player:@x` | Delete one player's record |
| `/stats replace-player from:@old into:@new` | Move a record to a new account, merging if it already has one |

`replace-player` carries the rating history across, so the new account inherits
the audit trail rather than starting blank.

---

## What is audited

Every one of these writes an audit row with the actor, the target, the before
and after values where applicable, and the running mode:

* Channel enable, disable, and configuration changes
* Queue creation, configuration, and deletion
* Moderator queue additions and removals, and queue clears
* Match launches, moderator reports, cancellations, substitutions, expiries
* Queue bans and releases
* Join phrase additions and clears
* Every rating adjustment and statistics reset

Audit writes never fail a command: if the audit insert fails it is logged as an
error and the action proceeds. Losing one audit row is bad; failing a
moderator's command because of it is worse.

---

## Irreversible actions

These cannot be undone. Everything else can be corrected.

| Action | Consequence |
| --- | --- |
| `/stats reset` | Every rating and history row in the pool is deleted |
| `/stats reset-player` | That player's record and history are deleted |
| `/queue delete` | The queue and its membership are deleted. Matches survive |
| `/channel disable` | The channel stops working. Configuration, ratings, and history are kept |

---

## Troubleshooting

| Symptom | Cause | Fix |
| --- | --- | --- |
| Commands do not appear | Not registered for this guild | In production, global commands can take up to an hour. In debug, check the guild is in the allowlist |
| "PUGbot is not enabled in this channel" | Channel never enabled, or disabled | `/channel enable` |
| "This channel has no queue yet" | Enabled but no queue | `/queue create` |
| Offline players are not removed | `GUILD_PRESENCES` not granted | Enable it in the Discord developer portal |
| A match is stuck in check-in | Timer job not running | Check the process is alive and `/ready` returns 200 |
| Ratings are not moving | Queue is not ranked | `/queue set-basics ranked:true` |
| A player cannot join, no reason given | Ban, live match elsewhere, or a role | `/noadds list`; check `queue-scope` and the access roles |
