# T-064 — Documentation: self-hosting guide, user guide, protocol docs site

Status: todo
Phase: 4
Size: S
Depends on: T-034, T-057
Paths: `docs/` (mdBook or plain markdown), `README.md`

## Goal
Someone outside the friend group can self-host and use it.

## Scope
- Root `README.md`: what it is, screenshots, quick start (server + client),
  feature list, status.
- Self-hosting guide (from T-034 README, expanded): install, TLS modes,
  ports, backup/restore, upgrading, troubleshooting (UDP blocked, NAT, MTU),
  running beside TeamSpeak.
- User guide: adding a server, PTT per OS incl. permissions, audio settings
  explained (what each processing toggle does and costs), roles for admins,
  keyboard shortcuts.
- Protocol reference generated from `.proto` comments (`protoc-gen-doc`)
  plus `02-protocol.md`, published for future bot authors.
- Contributing: conventions link, how tasks work, dev setup per OS
  (dependencies list).

## Out of scope
Marketing site.

## Acceptance criteria
- A reviewer unfamiliar with the project follows the self-hosting guide on
  a fresh VPS successfully (note who/when).
- Docs build in CI (link check).

## Notes from implementation
