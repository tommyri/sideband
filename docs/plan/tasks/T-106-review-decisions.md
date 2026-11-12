# T-106 — Decide the questions the review waves left open

Status: done
Phase: 4
Size: S
Depends on: —
Paths: `docs/design/decisions.md`, `docs/plan/`, `docs/security.md`

## Goal
Each question the 2026-11-09 review tasks left to the maintainer has a
recorded answer, and a follow-up task where the answer changes code.

Source: T-099, collected from the notes of T-048, T-083, T-085, T-088,
T-090, T-094 and T-095. In each case the code and the plan now agree on
today's behaviour; what is missing is the decision whether to keep it.

## Scope
Record one answer per item (here and, where it is a lasting rule, in
`docs/design/decisions.md` or the plan document named):

- **Ban and the member list** (T-094). A ban keeps the account in every
  `Welcome` and `MemberLeft` is reserved (`02-protocol.md` §3). Should a
  ban, or a future account disable, remove the member and send
  `MemberLeft`, with old messages falling back to "Unknown author"?
- **Owner** (T-090). Administrators are peers; there is no owner above
  them (`03-data-model.md`). T-090 changed the document instead of adding
  an owner column. Confirm, or open the task that adds one.
- **Real-time priority on Linux** (T-083). Promotion needs rtkit over D-Bus
  (`audio_thread_priority/with_dbus`), which the dependency policy would
  have to accept. Today the voice worker runs at normal priority on Linux.
- **Capture channel count** (T-088). Should `choose_format` prefer a
  device's full channel count when Input 2 or Average is selected (some ALSA
  `plug` devices also offer mono), and should the device list report input
  counts instead of offering Inputs 1–16?
- **PTT arming** (T-085, kept by T-095). A press for a call the 50 ms tick
  has not seen yet needs a release first. Keep or change?
- **Windows low-level hook fallback** (T-048). Implement the
  `WH_KEYBOARD_LL`/`WH_MOUSE_LL` fallback or waive it. T-048 stays
  `in-progress` until this is answered; a waiver makes it
  `awaiting-acceptance`.

## Out of scope
Implementing the answers; each one that changes code gets its own task.

## Acceptance criteria
- Every item above has an answer in this file's notes, and the documents it
  names say the same.
- `tasks/ACCEPTANCE.md` no longer lists this task, and T-048's status
  follows the hook answer.

## Notes from implementation

Answered by the maintainer on 2026-11-12. The lasting rules are in
`docs/design/decisions.md`, "Review decisions of 2026-11-12".

| Item | Answer | Follow-up |
|---|---|---|
| Ban and the member list | Keep: a banned member stays in every `Welcome` and their messages keep their name; `MemberLeft` stays reserved | none |
| Owner | Keep: administrators are peers, no owner above them (`03-data-model.md`) | none |
| Real-time priority on Linux | Accept rtkit over D-Bus (`audio_thread_priority/with_dbus`) | T-117 |
| Capture channel count | Fix both: report real input counts and open the full channel count for Input 2 or Average | T-118 |
| PTT arming | Keep: a press for a call the 50 ms tick has not seen needs a release first | none |
| Windows low-level hook fallback | Waive. T-048 becomes `awaiting-acceptance` | none |
| Bot token lifetime (raised by T-080) | No expiry: a bot token lasts until it is reset or revoked | none |
