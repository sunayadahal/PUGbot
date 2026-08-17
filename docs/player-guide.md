# Player guide

## Scope

How to play a pickup game with PUGbot. For setting one up, see the
[administrator guide](administrator-guide.md).

You need nothing but the commands below. If a command is missing, this channel
has not been set up for it — ask a server administrator.

## The short version

```
/add        join the queue
/who        see who is waiting
/remove     leave the queue
```

When the queue fills, the bot starts the match and tells you what to do next.

---

## Joining and leaving

### Join the queue

```
/add
```

The bot confirms and shows how full the queue is, for example `3/10`.

Your place expires after a while so a forgotten `/add` does not block the queue.
To choose how long, add the option:

```
/add expire:1800        keep my place for 30 minutes
/add expire:0           keep my place until I remove it
```

The server sets a maximum, so a very large value is capped.

**If joining is refused,** the reason is one of:

| Message | What to do |
| --- | --- |
| You are already in this queue | Nothing — you are in. |
| The queue is full | Wait; a place may open. |
| You are already in an active match | Finish or leave that match first. |
| You cannot queue until … | You have a timed queue ban. Ask a moderator. |
| You do not have the role required | Ask an administrator which role this queue needs. |

### Leave the queue

```
/remove
```

### See who is waiting

```
/who
```

---

## When the match starts

What happens depends on how the channel is configured. You may see any of these
steps, in this order.

### 1. Ready check

The bot posts a message with **Ready** and **Not ready** buttons and a
countdown. Press one. You can also type `/ready` or `/notready`.

If everybody presses Ready, the match continues immediately.

If somebody declines or the countdown runs out, the match is cancelled. Players
who were ready go back into the queue — the exact policy is up to the server,
but a player who pressed **Not ready** is never put back.

**To be marked ready automatically next time:**

```
/auto-ready seconds:600
```

This lasts for one match, or until the time runs out.

### 2. Team selection

One of four things happens, depending on the channel:

* **Captains pick.** Two captains take turns choosing players. When it is your
  turn the bot says so; pick with `/pick player:@someone`. Only the captain on
  the clock can pick, and the last remaining player is assigned automatically
  rather than making somebody click through a single option.
* **Teams are balanced by rating.** Nothing to do.
* **Teams are random.** Nothing to do.
* **No teams.** The bot lists the players.

**Captain commands:**

| Command | Effect |
| --- | --- |
| `/capfor team:1` | Claim an empty captain slot |
| `/capme` | Step down, if you have not picked anybody yet |
| `/pick player:@someone` | Pick, when it is your turn |

### 3. Map vote

If the channel votes on maps, the bot posts the candidates as buttons. Press
one. You can change your mind — the last press counts. The vote closes as soon
as everybody has voted, or when its timer runs out.

### 4. Playing

The bot posts the teams, the maps, and the server details, and sends you a
direct message if you have those switched on.

```
/teams          show my current match
/matches        show every active match in this channel
/server         show the server details again
```

---

## Reporting the result

```
/report result:...
```

Choose *My team won*, *The other team won*, *Draw*, or *Cancel the match*. You
can also press the buttons on the match message.

**Both teams must agree.** Your report is recorded, and the result becomes final
once somebody on the other team reports the same thing. If the two sides report
differently, the match is held for a moderator to settle — no single player can
decide a result.

---

## If you cannot play

### Ask for a replacement

```
/subme
```

The bot announces that you are looking for a substitute, and pings the channel's
promotion role if one is set.

### Take somebody's place

```
/subfor player:@someone
```

You inherit their team, and their captaincy if they had one.

---

## Your record

| Command | Shows |
| --- | --- |
| `/rank` | Your rating, rank, win/loss/draw record, streak, and last change |
| `/rank player:@someone` | Somebody else's |
| `/leaderboard` | The channel ranking |
| `/leaderboard page:2` | A later page |
| `/top` | The most active players |
| `/lastgame` | The most recent finished match |
| `/lastgame player:@someone` | Their most recent match |

Ratings only move in **ranked** channels. A cancelled or expired match never
changes anybody's rating.

Your rating carries a **± figure** — how sure the system is about it. It starts
wide and narrows as you play.

---

## Your settings

These apply to you across every channel unless noted.

| Command | Effect |
| --- | --- |
| `/switch-dms` | Turn match-start direct messages on or off |
| `/expire-default seconds:7200` | How long your queue place lasts by default; `0` means never |
| `/expire seconds:600` | Change the place you hold *right now* |
| `/auto-ready seconds:600` | Be marked ready automatically for the next match |
| `/allow-offline seconds:3600` | Stay queued even if you go offline |
| `/subscribe` | Get pinged when this channel's queue is promoted |
| `/unsubscribe` | Stop being pinged |

---

## Getting the queue noticed

```
/promote
```

Announces the queue and pings its promotion role. There is a cooldown, so this
cannot become spam; if you are too early the bot tells you how long to wait.

---

## Help

```
/help          how this channel's queue works
/commands      the command list
```

Errors are shown only to you, never to the channel.
