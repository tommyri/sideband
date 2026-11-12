# T-112 — Cap channels so one member cannot fill the Welcome

Status: done
Phase: 4
Size: S
Depends on: T-109
Paths: `crates/ss-server/src/limits.rs`, `crates/ss-server/src/handlers/`, `crates/ss-server/tests/`, `docs/security.md`, `docs/plan/03-data-model.md`

## Goal
No single member can make every `Hello` fail by creating content, so the
Welcome bound in `docs/security.md` §7 holds against a hostile member with
channel rights.

Source: T-109. It capped roles at 250 and measured the Welcome: a guild
channel with a full topic costs about 4,440 bytes, so about 230 such
channels fill the 1 MiB frame on their own. Anyone with `MANAGE_CHANNELS`
can create them, and then every member's `Hello` is refused.

## Scope
- Cap guild channels, or budget their topic bytes, or both, so that at the
  caps (with roles at 250) the longest-string Welcome still leaves room for
  members. Pick the numbers from T-109's table in `tests/welcome_bound.rs`
  and state them in `docs/security.md` §7.
- Refuse creation past the cap with a clear error, inside the write
  transaction, as `CreateRole` does.
- Decide whether existing servers over the cap keep their channels (refuse
  only new ones) and record it.

## Out of scope
Member paging, which would be a protocol change.

## Acceptance criteria
- Test: creating a channel past the cap is refused.
- `welcome_bound` measures a Welcome at the channel and role caps with
  every string at its longest and asserts the stated member headroom.

## Notes from implementation

Each test failed first for the reason given.

| Item | Test | Before the fix |
|---|---|---|
| Channel cap | `channels::creating_a_channel_past_the_cap_is_refused` | the 501st channel (a category, then a voice channel) was created |
| Existing server over the cap | `channels::a_server_over_the_channel_cap_keeps_and_edits_its_channels` | a create at 505 channels succeeded |
| Topic budget | `channels::channel_topics_share_one_byte_budget` | a one-byte topic past 65,536 B was accepted |
| Existing server over the budget | `channels::a_server_over_the_topic_budget_can_shrink_but_not_grow` | a topic that grew past the budget was accepted |
| Headroom at the caps | `welcome_bound::a_welcome_at_the_channel_and_role_caps_leaves_member_headroom` | with the count cap alone (every topic full) the `Welcome` was 2,963,884 B |

**Caps.** Both a count and a topic budget, in `limits.rs`:
`MAX_CHANNELS = 500` live guild channels, categories included, and
`CHANNEL_TOPIC_BYTES = 65,536` UTF-8 bytes of topic across them. Deleted
channels and DMs do not count. Both are checked in the `CreateChannel` or
`UpdateChannel` write transaction from `channels::list`, as `CreateRole`
counts roles, and refused with `FORBIDDEN`: "the server already has the
maximum number of channels", "the server's channel topics would pass their
total length limit". No protocol change, no new error code.

Why both: a full-topic channel costs 4,440 B but a topicless one with the
longest name and every voice setting only 341 B (newly measured), so almost
all of a channel's worst case is its topic. A count cap alone would have to
be about 70 channels to leave room for 1,000 longest members. A budget
alone would leave the count, and so the 341 B part and the category-delete
event burst, unbounded. 500 channels is far past what a self-hosted guild
uses, and deleting a category with 499 children publishes 500 events,
under the 1,024-event bus. 64 KiB is 64 full ASCII topics, or about 130 B
on every channel at the cap.

**Numbers.** Per channel: 341 B, 44 B read state, 3 B topic framing, plus
its topic bytes. At both caps roles and channels take at most
250·435 + 500·388 + 65,536 = 368,286 B, leaving at least 501,714 B of the
870,000 B budget in section 7 for members: about 1,100 longest members
holding five roles, or about 6,000 typical members holding two. Measured
at both caps, every string longest, the budget spread over all 499
channels under the seeded category (the worst split, most framing), 256
members online with full activities and seats, a read state on every
channel, and 1,000 longest members holding five roles: 985,514 B, 63,062 B
spare. Section 7's bound is now 359·M + 435·R + 18·G + 388·C + T ≤ 870,000
(T topic bytes).

**Growth paths.** Creating a channel or category: the count. Creating with
a topic, or setting a longer topic: the budget, in bytes, not characters
(four-byte characters count four). Renaming: names stay at 64 characters,
inside the 341 B per channel. Kind changes and voice settings: fixed size.
Moves: position and parent only. Categories are in the snapshot and count
as channels; their topics count too. Delete and recreate: deleted channels
are left out of the `Welcome` and free their slot and bytes.

**Existing servers over a cap.** They keep everything. Only a create past
the count, or a create or update that adds topic bytes past the budget, is
refused. Renames, moves, deletes, and topic edits that keep or reduce the
channel's topic bytes still work, so an over-budget server can always get
back under. Recorded in `docs/security.md` §7 and `03-data-model.md`.

**Tests.** `welcome_bound` gained a `Topics` mode (full, absent, a spread
budget) and a topicless channel cost (stated ≤ 345 B). The longest example
in `the_stated_welcome_examples_fit_one_frame` (50 channels with full topics,
204,800 B of topic) is no longer reachable, so that test keeps the typical
example and the headroom test replaces the longest one.

**Files outside the Paths.** This file and `INDEX.md`. Nothing else.

**Open questions.**
- Members and grants are still uncapped; a guild of more than about 1,000
  longest or 8,000 typical members still refuses every `Hello`. Member
  paging is the fix (a protocol change).
- How the desktop presents the two new `FORBIDDEN` messages was not
  checked (out of the Paths).
