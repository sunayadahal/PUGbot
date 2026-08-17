# Requirements traceability

## Scope

Maps each requirement in
[`FEATURE_IMPLEMENTATION_PLAN.md`](../FEATURE_IMPLEMENTATION_PLAN.md) to the
code that implements it and the test that verifies it. Reviewers use this to
check coverage; maintainers update it when a requirement's status changes.

**Verification key**

| Symbol | Meaning |
| --- | --- |
| ✅ | Implemented and covered by a named automated test |
| ⚠️ | Implemented, but verification is partial — the limitation is stated |
| ❌ | Not implemented — the reason is stated |

Test names below are real and can be run individually:

```bash
cargo test every_pick_order_terminates_with_full_teams
```

Database tests need `PUGBOT_TEST_DATABASE_URL`; see the
[README](../README.md#testing).

---

## Reference parity checklist

The specification's own checklist, item by item.

| # | Requirement | Status | Implementation | Verification |
| --- | --- | --- | --- | --- |
| 1 | Exactly one configurable queue per enabled channel, enforced by database and service layer | ✅ | `UNIQUE (channel_id)` on `queues`; `Store::create_queue` | `a_channel_can_only_ever_own_one_queue` |
| 2 | Channel queue promotion subscriptions | ✅ | `ModerationService::subscribe` / `unsubscribe` | `queued_players_render_as_mentions_in_join_order` (rendering); schema exercised by integration setup |
| 3 | Auto-start, manual start, and membership moderation | ✅ | `QueueService::start_match`, `force_add`, `force_remove`, `clear` | `autostart_only_fires_on_a_full_queue_with_the_setting_on`, `a_check_in_everyone_passes_moves_on_to_the_match` |
| 4 | Presence and timer-based removal, auto-ready, DM preferences | ✅ | `queue::should_remove_for_presence`, `Handler::presence_update`, `MatchService::launch` | `presence_removal_respects_the_opt_out`, `presence_removal_ignores_the_opt_out_when_the_channel_forbids_it`, `afk_removal_is_configured_separately_from_offline_removal`, `auto_ready_expires`, `expired_queue_members_are_swept_and_the_sweep_is_idempotent` |
| 5 | Ready-check with timeout and decline policies | ✅ | `domain::checkin` | `a_decline_aborts_immediately_when_configured`, `timeout_applies_the_return_policy`, `a_decliner_is_never_returned_to_the_queue`, `failure_lists_partition_the_roster`, `a_check_in_that_times_out_returns_players_to_the_queue` |
| 6 | Draft, matchmaking, random, and no-team modes | ✅ | `domain::draft`, `domain::teams`, `MatchService::enter_team_formation` | `every_pick_order_terminates_with_full_teams`, `balanced_split_is_optimal_for_two_teams`, `random_teams_partition_the_roster`, `a_captain_draft_persists_and_reloads_correctly` |
| 7 | Configurable captains, pick order, names, and emojis | ✅ | `CaptainMode`, `PickOrder`, `QueueSettings::team_label` | `role_and_rating_prefers_captain_role_holders_over_higher_ratings`, `fair_pair_picks_the_closest_ratings`, `random_with_role_preference_never_skips_a_role_holder`, `captain_selection_never_returns_duplicates`, `pick_order_parses_and_round_trips` |
| 8 | Map pool, recent-map avoidance, and map voting | ✅ | `domain::maps` | `recent_maps_are_avoided_when_the_pool_allows`, `cooldown_relaxes_rather_than_starving_a_small_pool`, `the_most_voted_map_wins`, `deterministic_tie_break_prefers_the_first_candidate`, `a_map_vote_resolves_when_everyone_has_voted` |
| 9 | Match announcements, server text, streams, DMs, and substitutions | ⚠️ | `discord::embeds`, `MatchService::send_start_dms`, `substitute` | Rendering: `map_vote_buttons_are_split_into_rows_of_five`, `long_field_values_are_truncated_on_a_character_boundary`. Substitution: `substitution_preserves_the_captain_slot`, `a_substitute_starts_pending`. **Announcement delivery is not verified against a live gateway.** Stream listing is a configuration flag with no Discord-side implementation |
| 10 | Win/draw/cancel reports and moderator controls | ✅ | `domain::report`, `MatchService::report`, `moderator_report` | `matching_reports_from_both_teams_agree`, `opposing_claims_are_disputed`, `teammates_contradicting_each_other_is_a_dispute`, `three_teams_all_have_to_agree`, `a_disputed_result_waits_for_a_moderator` |
| 11 | Flat, Glicko-2, and TrueSkill ratings with full history | ✅ | `domain::rating::{flat, glicko2, trueskill}` | `every_adapter_rates_a_two_versus_two_win`, `every_adapter_rejects_a_single_team_or_duplicate_player`, `helper_functions_match_the_published_example`, `an_upset_moves_ratings_more_than_an_expected_result`, `a_ranked_match_is_rated_exactly_once` |
| 12 | Rank roles/nicknames, decay, leaderboards, profiles, and stats | ⚠️ | `RatingService`, `nickname_prefix`, `ChannelSettings::rank_for` | `rank_lookup_picks_the_highest_matching_floor`, `decay_pulls_toward_the_initial_rating_and_never_past_it`, `nicknames_are_truncated_to_the_discord_limit`. **Rank *role* assignment and nickname *writing* are not implemented** — both need gateway calls; `/nick` reports the prefix instead |
| 13 | Timed queue bans and player join phrases | ✅ | `ModerationService::ban`, `add_phrase` | `an_active_ban_blocks_joining_until_it_is_released`, `an_active_ban_blocks_joining_but_an_expired_one_does_not` |
| 14 | Channel/queue access roles and staff permission levels | ✅ | `domain::permissions`, `JoinRequest::evaluate` | `access_roles_are_enforced`, `configured_roles_grant_their_level`, `native_discord_permissions_grant_their_level_without_a_configured_role`, `the_bot_owner_outranks_everyone`, `levels_are_ordered_and_inclusive` |
| 15 | Localization, help, audit logs, persistence, and restart recovery | ✅ | `localization`, `AppContext::audit`, `jobs::recover_on_startup` | `every_catalog_has_the_same_keys_as_english`, `every_catalog_preserves_placeholders`, `all_eight_target_languages_are_installed`, `moderation_actions_are_audited_with_their_mode`, `restart_recovery_resolves_deadlines_that_passed_while_the_process_was_down` |

---

## Match state model invariants

| Invariant | Status | Verification |
| --- | --- | --- |
| A user appears at most once in a queue | ✅ | `PRIMARY KEY (queue_id, user_id)`; `the_same_player_cannot_join_twice` |
| A user appears at most once in a match roster | ✅ | `PRIMARY KEY (match_id, user_id)`; `a_duplicate_roster_is_refused_at_construction` |
| A queued player cannot be active in another match in scope | ✅ | `a_player_cannot_be_in_two_live_matches_in_one_channel`, `a_finished_match_releases_its_players` |
| A player belongs to at most one team in a match | ✅ | `balanced_split_partitions_every_player_exactly_once`, `swap_refinement_never_breaks_the_partition` |
| Only the active captain can make the current pick | ✅ | `only_the_active_captain_can_pick` |
| A completed match cannot transition again | ✅ | `completed_match_cannot_be_reopened`, `terminal_states_have_no_successors` |
| Rating updates generated once per finalised ranked match | ✅ | `a_ranked_match_is_rated_exactly_once`, `the_rating_history_index_blocks_a_duplicate_write` |
| Concurrent transitions rejected | ✅ | `optimistic_locking_rejects_a_stale_transition` |
| Queue capacity holds under concurrency | ✅ | `concurrent_joins_never_exceed_the_queue_size` |

---

## Debug/production separation

Every row of the specification's isolation table.

| Requirement | Status | Verification |
| --- | --- | --- |
| Separate `PUGBOT_DEBUG_*` / `PUGBOT_PRODUCTION_*` sources | ✅ | `debug_config_loads_from_its_own_prefix`, `production_config_loads_from_its_own_prefix` |
| Never fall back between modes | ✅ | `debug_startup_fails_when_only_production_credentials_are_present`, `production_startup_fails_when_only_debug_credentials_are_present`, `neither_mode_falls_back_to_the_other_for_any_single_value` |
| Startup prints mode, application ID, database, guilds | ✅ | `the_startup_summary_carries_no_credentials`; verified manually against the running binary |
| Debug rejects interactions outside its allowlist | ✅ | `debug_rejects_guilds_outside_the_allowlist`, `debug_requires_a_guild_allowlist` |
| Separate Discord applications | ✅ | `a_shared_token_between_modes_is_rejected` |
| Separate databases | ✅ | `a_shared_database_between_modes_is_rejected` |
| Guild cannot be in both allowlists | ✅ | `a_guild_in_both_allowlists_is_rejected_for_debug` |
| `mode` in logs, audit events, job locks | ✅ | `moderation_actions_are_audited_with_their_mode`, `job_leases_stop_two_workers_running_the_same_sweep` |
| Verbose debug logs, operational production logs | ✅ | `log_defaults_are_verbose_in_debug_and_quiet_in_production` |
| Secrets redacted | ✅ | `secrets_are_redacted_in_debug_and_display_output`, `database_urls_are_redacted_for_logging` |
| Debug reset cannot execute against production | ✅ | Cargo feature gate, runtime mode check, acknowledgement flag; CI asserts the release binary lacks the subcommand |
| Fake clock and repository doubles for deterministic tests | ✅ | `fake_clock_advances_only_when_told`; every timing test uses `FakeClock` |
| Behavioural logic identical between modes | ✅ | By construction — mode affects configuration, registration scope, and logging only |
| CI uses ephemeral databases | ✅ | `.github/workflows/ci.yml` provisions a `postgres:17` service container |

---

## Delivery phases

| Phase | Exit criterion | Status |
| --- | --- | --- |
| 1 — Foundation and casual queues | Two guilds independently configure queues and complete unranked matches | ⚠️ Implemented and integration-tested; not exercised against a live gateway |
| 2 — Check-in, drafts, maps, recovery | A restart during check-in, draft, or active play resumes without duplicated or lost state | ✅ `restart_recovery_resolves_deadlines_that_passed_while_the_process_was_down`, `a_captain_draft_persists_and_reloads_correctly` |
| 3 — Ranked play and statistics | Repeated or concurrent result submissions cannot duplicate history or ratings | ✅ `a_ranked_match_is_rated_exactly_once`, `the_rating_history_index_blocks_a_duplicate_write` |
| 4 — Moderation, localization, hardening | Runbook, tested restore, monitoring, permissions review complete | ⚠️ [Runbook](operations.md) and [restore procedure](operations.md#backups) documented; monitoring and metrics implemented. **Load and concurrency testing not performed; restore procedure documented but not executed** |

---

## Not implemented

Stated plainly, with reasons.

| Item | Reason |
| --- | --- |
| Rank role assignment and nickname writing | Both need gateway calls from the adapter. `/nick` reports the prefix it would apply; the rank tier's `role_id` is stored and configurable but never synchronised |
| Streaming player listing | `show-streams` is stored and configurable; no Discord-side implementation reads presence for stream state |
| Web configuration UI | Out of scope by decision 7 — the specification itself noted it was absent from the reference implementation |
| Prefix/message commands | Out of scope by decision 2 — slash commands only |
| Load and concurrency testing | Not performed. Concurrency *correctness* is tested (`concurrent_joins_never_exceed_the_queue_size`); throughput under load is not |
| Live gateway end-to-end testing | Needs a real bot token. Command definitions, dispatch, and rendering are unit-tested; the first real connection is unproven |
| Multi-replica deployment | Job leases and optimistic locking are designed for it; untested |

---

## Test inventory

| Suite | Count | Requires a database |
| --- | --- | --- |
| Unit (`src/**`) | 211 | No |
| Integration (`tests/integration.rs`) | 20 | Yes — skips with a message otherwise |
| Doctests | 6 | No |
| **Total** | **237** | |

```bash
cargo test                       # unit and doctests; database tests skip
cargo test --all-features        # all, given PUGBOT_TEST_DATABASE_URL
```

---

## Two defects found by these tests

Recorded because they are the argument for the tests existing.

**Queue capacity race.** `concurrent_joins_never_exceed_the_queue_size` drove ten
simultaneous joins at a four-slot queue and all ten were accepted. The capacity
check and the insert were separate statements, so every caller passed the check
before any of them inserted. Fixed by taking a row lock on the queue inside
`Store::add_queue_member_atomic`, making check-and-insert atomic.

**Embed truncation overrun.** `long_field_values_are_truncated_on_a_character_boundary`
showed the truncated string exceeding Discord's 1024-character field limit: the
ellipsis was appended *after* the budget was spent. Fixed by including it in the
budget.
