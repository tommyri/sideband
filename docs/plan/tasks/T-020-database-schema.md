# T-020 — SQLite schema, migrations, repository layer

Status: done
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

- The database file is `data-dir/socialspeak.db`. `ss-server` applies migrations
  before it listens. `--check-migrations` only compares the file with the
  embedded SQL: a missing file or a pending migration exits with an error, and
  a checksum mismatch does too. It does not apply SQL.
- `db::Pool` keeps one write connection and a read pool of up to four. Every
  connection sets WAL, `synchronous=NORMAL`, `foreign_keys=ON`, and
  `busy_timeout=5000`. Repository functions take `&mut SqliteConnection`, so a
  caller can use the write guard or a connection acquired from the read pool.
- Queries use sqlx 0.9's runtime API with `'static` SQL literals. sqlx 0.9
  rejects dynamic SQL strings. There is no `query!` cache, and
  `cargo sqlx prepare` has nothing to record. `sqlx::migrate!` embeds the
  migration file, so CI builds without `DATABASE_URL`.
- Seeded roles use fixed ids: `@everyone` is 16 zero bytes ending in `01`
  (`EVERYONE_ROLE_ID`, `Permissions::EVERYONE`, position 0, `is_default`) and
  `Admin` ends in `02` (`ADMIN_ROLE_ID`, `Permissions::ADMINISTRATOR`,
  position 1). `server_settings` starts as name `SocialSpeak`, empty
  description and icon, and `registration_mode=invite`.
- Message and channel removal sets `deleted_at`. Pages skip deleted messages.
  `page_before` returns the newest `limit` visible rows older than the cursor,
  ascending. `page_around` keeps a visible anchor and fills neighbors, with at
  most half of the spare slots older than it. A missing anchor returns the
  nearest visible ids and prefers the older id on a tie. `limit = 0` is empty.
- `link_previews.status` is `0` pending, `1` ready, `2` failed. The primary
  key is the SHA-256 of the URL string. The voice spike still does not read
  these tables.
