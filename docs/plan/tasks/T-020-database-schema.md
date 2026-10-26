# T-020 — SQLite schema, migrations, repository layer

Status: todo
Phase: 2
Size: M
Depends on: T-003
Paths: `crates/ss-server/migrations/`, `crates/ss-server/src/db/`

## Goal
The full schema from `03-data-model.md` as sqlx migrations, plus thin typed
repository functions, so every later server task only writes domain logic.

## Scope
- `0001_init.sql` with all tables and indexes from `03-data-model.md`;
  seed rows: `@everyone` role, `Admin` role, `server_settings` defaults.
- `db::Pool` wrapper: opens with WAL, `synchronous=NORMAL`, `foreign_keys=ON`,
  `busy_timeout=5000`; one write connection + read pool; runs migrations on
  start; `--check-migrations` CLI flag.
- Repositories (one module per aggregate): `users`, `sessions`, `invites`,
  `roles`, `channels`, `messages` (insert, edit, soft delete, page before id,
  around id), `attachments`, `link_previews`, `read_states`, `bans`,
  `audit_log`, `webhooks`, `settings`. Functions take `&mut SqliteConnection`
  or `&Pool`; return domain structs from `ss-core`/server `model` module.
- Test helper `db::test_pool()` creating a temp file DB with migrations.

## Out of scope
Business rules, HTTP, protocol.

## Acceptance criteria
- `sqlx::migrate!` runs clean on an empty file and is idempotent on re-run.
- Repository tests for each module: insert/read/update/delete happy paths and
  the pagination queries (`before`, `around`, limit boundaries).
- `cargo sqlx prepare` offline data committed so CI builds without a DB.

## Notes from implementation
