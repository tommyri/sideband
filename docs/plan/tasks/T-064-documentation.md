# T-064 — Documentation: self-hosting guide, user guide, protocol docs site

Status: awaiting-acceptance
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

Implemented on 2026-11-12, plain Markdown (no mdBook).

- **Guides.** `docs/self-hosting.md` (method choice, a ports table with the
  `init` and Compose defaults, first start and the owner invite, running
  server commands under the systemd dynamic user, certificate modes,
  configuration, backups/upgrades/monitoring, troubleshooting for blocked
  UDP, NAT, MTU and VPNs, buffers, start failures, TeamSpeak).
  `docs/user-guide.md` (adding a server, talking, every audio setting with
  its default and cost, global PTT per OS, keyboard shortcuts, chat and
  notifications, administrator panels and roles). Both link to the document
  that holds each fact instead of copying it: `deploy/README.md` stays the
  source for install, TLS, backup, upgrade and firewall commands,
  `docs/release.md` for verification, `docs/security.md` for trust and
  permissions, `config.toml` (written by `init`) for settings, `--help` for
  commands. Facts were checked against the code and `ss-server`/`ss-cli`
  `--help` at `408ff03`.
- **Moved.** The usage paragraphs of `docs/testing-app.md` (sessions, audio
  settings, PTT, shortcuts, global shortcuts, local tests) moved to the user
  guide, leaving a link; its install, diagnostics and evidence sections stay.
  The README's development setup and CI policy moved to `CONTRIBUTING.md`,
  which now also explains how tasks work. The README was rewritten around
  what it is, features, status, a quick start and a documentation map;
  screenshots are a TODO comment listing the six wanted.
- **Protocol reference.** `docs/protocol/README.md` is the landing page for
  bot and client authors; `docs/protocol/messages.md` is generated from the
  `.proto` comments by `scripts/proto-doc.py` (standard-library Python, tests
  in `scripts/proto-doc.test.py`, `just proto-doc` / `just proto-doc-check`)
  instead of `protoc-gen-doc`, which the project does not install. The parser
  accepts only the proto3 subset the schema uses and fails on anything else.
- **Link check.** `scripts/check-doc-links.py` (tests in
  `scripts/check-doc-links.test.py`, `just doc-links`) checks every relative
  link, image and `#anchor` in the Markdown Git tracks, with GitHub's heading
  slugs and exact-case paths. External URLs are not checked. Both checks run
  in `just plan-check` (about half a second together) and in one manual
  `ci.yml` step. At this commit the repository has no broken relative link.
- **Found, not fixed (other owners).** The desktop app assumes port 5781
  when an address has no port, while the server defaults to UDP 443
  (`apps/desktop/src/features/onboarding/validation.ts`, T-108's paths).
  `ss-server init` writes `bind_http = 0.0.0.0:4430`, so the systemd install
  serves HTTPS on 4430 while `deploy/README.md` opens TCP 443 and
  `02-protocol.md` says "HTTPS on TCP 443". The guides document both.
- **Unverified.** Nobody has followed the self-hosting guide on a fresh VPS
  (acceptance row). The `systemd-run` pattern for admin commands is the
  backup procedure's, not yet run on a VPS. Windows and Linux steps in the
  user guide are marked unverified, matching the open acceptance rows.
