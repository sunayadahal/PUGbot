# Glossary

## Scope

Terms used across PUGbot's documentation and code, including the ones this
project uses in a narrower sense than common usage.

---

**Actor** — the caller of a command, reduced to what the permission policy
needs: their user ID, their roles, their native Discord permissions, and whether
they are a bot owner. See `domain::permissions::Actor`.

**Advance** — the single function through which every match state change passes.
It pushes a match as far forward as the current facts allow and then stops.
Commands change a fact and call it; the timer job calls it too. See
`MatchService::advance`.

**Audit event** — an append-only record of a configuration change, moderator
action, ban, reset, or rating adjustment. Carries the actor, the target, before
and after values where applicable, and the running mode.

**Autostart** — launching a match automatically when the queue reaches its
configured size.

**Captain** — a player who picks teammates in a captain draft. Occupies the
first roster slot of their team.

**Check-in** — the optional ready-check between a queue filling and teams being
formed. See also *return policy*.

**Cooldown** — two unrelated things: the minimum gap between `/promote` calls,
and the number of recent matches whose maps are avoided when picking new ones.
Context distinguishes them.

**Debug mode** — one of the two operating modes. Uses its own Discord
application, its own database, and an enforced guild allowlist. Not a verbosity
setting, and not a staging copy of production. See *mode*.

**Deviation** — how uncertain the rating system is about a rating, shown to
players as the `±` figure. Starts wide and narrows with play. Grows back during
inactivity. Called *sigma* in TrueSkill and *RD* in Glicko.

**Draft** — the process of captains alternately picking players. Also the value
holding that state, rebuilt from the database on every command rather than kept
in memory.

**Expiry** — two unrelated things: when a player's queue slot lapses, and when an
unreported match is abandoned. A match that expires is never rated.

**Flat rating** — the default rating system: a fixed change per result, before
win/loss scaling and streak multipliers.

**Glicko-2** — a rating system tracking rating, deviation, and volatility.
Implemented in full, including the volatility iteration.

**Guild** — Discord's internal name for what the user interface calls a server.
Used throughout the code because it is what the API uses.

**Idempotent** — safe to repeat. Every background sweep and every state
transition in PUGbot is idempotent, which is what makes retries, double-clicks,
and restarts harmless.

**Live match** — a match that has not reached a terminal state. A live match
occupies its players, blocking them from queueing within the configured scope.

**Locale** — the language a channel's messages are rendered in. Eight are
installed; an unknown one falls back to English.

**Mode** — debug or production. Selected by a required command-line argument
with no default. The two read entirely separate environment variables and never
fall back to one another.

**Optimistic locking** — the concurrency strategy for match transitions. Each
match carries a version; an update that names a stale version is rejected rather
than applied. The loser of a race gets a conflict, not a corrupted match.

**Pick order** — the pattern in which captains pick, written as letters: `ABBA`
means team A, then B twice, then A. Repeats if more picks are needed. A
preference, not a constraint — the turn passes if the named team is full.

**PUG** — pickup game. An ad-hoc match assembled from whoever is available,
rather than between standing teams.

**Queue** — the list of players waiting in one channel. A channel owns exactly
one, enforced by the database.

**Rating pool** — the set of rating records a channel reads and writes. Normally
the channel's own, but a channel may share another's so two queues feed one
ladder.

**Return policy** — what happens to players when a check-in fails:
`ready_only`, `ready_and_pending`, or `none`. A player who declined is never
returned, under any policy.

**Roster** — every player on a match, including substitutes who have since left.
Distinct from a *team*, which is one side of it.

**Scope** — see *queue scope*.

**Queue scope** — whether a live match anywhere in the guild blocks a player from
queueing, or only one in the same channel. Configurable per channel; defaults to
guild.

**Snapshot** — the copy of a queue's settings stored on each match at launch, so
later edits to the queue cannot change how a historical match is interpreted.

**Streak** — a player's current run: positive for consecutive wins, negative for
losses, reset to zero by a draw. Optionally amplifies rating changes.

**Terminal state** — `COMPLETED`, `CANCELLED`, or `EXPIRED`. A match in one
cannot transition again without an audited administrative correction, which is
modelled as a new record rather than a transition.

**TrueSkill** — a rating system tracking a Gaussian per player. Implemented as
the closed-form two-team update, with a draw margin derived from the configured
draw probability.

**Volatility** — Glicko-2's measure of how erratic a player's results are.
Unused by the other systems.
