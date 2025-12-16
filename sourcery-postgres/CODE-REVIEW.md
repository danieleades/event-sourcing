# Code Review: `sourcery-postgres`

Scope: `sourcery-postgres` as currently implemented in `sourcery-postgres/src/lib.rs` (single-file crate providing `PostgresEventStore` and a small schema helper).

## Executive summary

This crate is a clean, minimal `sqlx`-based Postgres implementation of `sourcery_core::store::EventStore`. The transactional append path is sound (row-locking a per-stream record, inserting events, then updating stream head in a single DB transaction) and matches the design described in `STORES.md`.

The main gaps are around operability and edge-case correctness:

- Schema management is “create-if-missing” only (no versioned migrations, no transactional DDL, no guidance for production rollout).
- There are no backend-specific tests (and no integration tests in this crate at all), so behavioral regressions will be hard to catch.

## What’s solid

- **Transaction safety**: `append` / `append_expecting_new` perform all critical operations in a single DB transaction and commit only after `es_streams.last_position` is updated.
- **Concurrency control**: Stream-level serialization via `SELECT ... FOR UPDATE` on `es_streams` correctly prevents interleaving writers on the same aggregate stream.
- **Injection safety**: Dynamic queries are built with binds (`sqlx::QueryBuilder`), not string interpolation.
- **Efficient batch insert**: Multi-row `INSERT ... VALUES ... RETURNING position` avoids N round trips when appending event batches.
- **Simple, explicit concurrency model**: `es_streams.last_position` is a single per-stream head used for optimistic concurrency checks.

## Findings & recommendations

### 2) Schema/migrations: only “create-if-missing” DDL (medium-high)

`PostgresEventStore::migrate` creates tables/indexes with `IF NOT EXISTS` and returns `sqlx::Error`.

Pros:
- Very easy to get started.
- Works well for initial development and empty databases.

Risks:
- No mechanism to evolve the schema once deployed (adding columns, changing indexes, data migrations).
- Running `CREATE INDEX` during application startup on a large table can block writes/reads.
- DDL is not wrapped in a transaction, so partial schema application is possible if a later statement fails.

Recommendations:

- Introduce versioned migrations (e.g., `sqlx::migrate!()` + `migrations/`), or document a clear path for future schema changes.
- If keeping the current approach, consider:
  - Wrapping the DDL sequence in an explicit DB transaction for atomicity.
  - Documenting that index creation on large datasets should be done offline / with `CONCURRENTLY` (and that `CONCURRENTLY` has restrictions).

Proposed solution (sketch):

- Add `sourcery-postgres/migrations/` and switch `migrate()` to `sqlx::migrate!().run(&self.pool).await?`.
- Put initial schema in `0001_init.sql` and add future migrations as additional numbered files.
- Document operational guidance: run migrations as a separate deployment step; use `CREATE INDEX CONCURRENTLY` for new indexes on large tables.

### 3) Query strategy: `UNION ALL` can duplicate rows on overlapping filters (medium)

`load_events` uses `UNION ALL` with one `SELECT` per filter. If a caller provides overlapping filters (e.g., same `event_kind` twice, or one filter that subsumes another), the same row can appear multiple times and will be applied multiple times by projections/repositories.

This may be “fine” if the library guarantees filters are non-overlapping, but the store currently relies on that property implicitly.

Recommendations:

- Document the expectation (“filters must be non-overlapping”) on `EventStore::load_events` usage, or
- Add deduplication by `position` (or `(aggregate_kind, aggregate_id, position)`), either in SQL (`UNION` instead of `UNION ALL`, though that has a cost) or in Rust after the fetch (linear scan if already sorted by position).

Proposed solution (sketch):

- Pre-deduplicate filters in Rust (exact duplicates): use a `HashSet<(event_kind, aggregate_kind, aggregate_id, after_position)>` before building SQL.
- After fetching, de-dup defensively by `position` (safe because `position` is globally unique): if events are already sorted, a `HashSet<i64>` or a `Vec<bool>`-like structure isn’t needed; a `HashSet` is fine and the path only triggers with overlapping filters.
- Optional: replace `UNION ALL` with `UNION` (SQL-level de-dup) if filter counts are small and simplicity is preferred over maximum performance.

### 4) Indexing: missing composite index for common per-aggregate loads (medium)

Current indexes:
- `(event_kind, position)`
- `(aggregate_kind, aggregate_id, position)`

Depending on query shapes, Postgres may need to scan many rows:
- Aggregate rebuild loads often filter on `(aggregate_kind, aggregate_id)` and multiple `event_kind`s.
- Projection loads often filter on `event_kind` and `position > N`.

Recommendations:

- Consider adding an index to match the most common “aggregate rebuild” predicate if needed in practice, e.g.:
  - `(aggregate_kind, aggregate_id, event_kind, position)` or `(aggregate_kind, aggregate_id, position, event_kind)`
- Keep it data-driven: verify with `EXPLAIN (ANALYZE, BUFFERS)` on representative workloads before adding indexes (indexes cost write amplification).

Proposed solution (sketch):

- Add an optional migration that introduces a composite index tuned to the dominant workload, e.g.:
  - For rebuilds: `CREATE INDEX CONCURRENTLY es_events_stream_kind_pos ON es_events(aggregate_kind, aggregate_id, event_kind, position);`
  - For projections-by-kind: keep `(event_kind, position)` as-is; consider `INCLUDE (aggregate_kind, aggregate_id)` if you frequently need those columns and want index-only scans.
- Capture recommended `EXPLAIN` queries in docs/examples so users can validate on their dataset.

### 5) Append batch size / parameter limits (medium)

The batch insert binds 5 parameters per event (`aggregate_kind`, `aggregate_id`, `event_kind`, `data`, `metadata`). Postgres has a hard limit on the number of bind parameters per statement; very large batches will error.

Recommendations:

- Document an expected/maximum batch size, or
- Chunk large batches in `append` / `append_expecting_new` (while still being atomic by keeping the same DB transaction).

Proposed solution (sketch):

- Add an internal constant like `const MAX_EVENTS_PER_APPEND: usize = 5_000;` (or compute from Postgres’ 65,535 parameter limit / 5 binds per event).
- In `append`/`append_expecting_new`, split `events` into chunks and execute one multi-row insert per chunk within the same DB transaction, tracking the last returned position from the final chunk before updating `es_streams.last_position`.

### 6) Ergonomics: cloning and construction helpers (low-medium)

`PostgresEventStore<C, M>` is simple, but a couple of small tweaks improve usability:
- `Clone` (when `C: Clone`) so callers can share the store easily.
- Optional construction helpers for common codec/metadata choices (if this crate wants to be batteries-included).

Recommendations:

- Implement `Clone` for `PostgresEventStore<C, M>` when `C: Clone`.
- Optionally add `impl PostgresEventStore<JsonCodec, serde_json::Value> { ... }` style constructors (only if it aligns with how other crates expose codecs/metadata).

Proposed solution (sketch):

- `#[derive(Clone)]` on the store struct (works because `PgPool: Clone`; gate on `C: Clone` by deriving and adding bounds as needed).
- Add minimal helper constructors only if there is an established “default codec” pattern elsewhere in the workspace.

### 7) Observability: no `tracing` spans/events (low)

The crate depends on `tracing` (with `attributes`), but `src/lib.rs` does not currently emit spans/events.

Recommendations:

- Add `#[tracing::instrument]` on `migrate`, `append`, `append_expecting_new`, and `load_events` (carefully `skip`ping large payloads like `events`/`data`), and include identifying fields (`aggregate_kind`, maybe a hash/length of `aggregate_id`).

Proposed solution (sketch):

- Annotate methods, e.g. `#[tracing::instrument(skip(self, events), fields(aggregate_kind, aggregate_id = %aggregate_id, events = events.len()))]`.
- For `load_events`, record `filters.len()` and potentially the set of `event_kind`s (but avoid dumping full filter contents if it can be large).
- Consider emitting a debug log on conflicts (expected vs actual) to simplify diagnosing concurrency issues.

### 8) Constraints/invariants: `es_streams.last_position` is the single source of truth (low)

Correctness of concurrency checks relies on `es_streams.last_position` being accurate. Without a foreign key from `es_events` to `es_streams`, and without a mechanism to repair/verify `last_position`, manual DB edits or future code changes could create mismatches.

Recommendations:

- Consider adding a FK `(aggregate_kind, aggregate_id) -> es_streams` to prevent orphan events.
- Consider adding a debug/admin check/repair query (not necessarily in the library API) to recompute `last_position` from `max(es_events.position)` if needed.

Proposed solution (sketch):

- Add to schema/migration:
  - `ALTER TABLE es_events ADD CONSTRAINT es_events_stream_fk FOREIGN KEY (aggregate_kind, aggregate_id) REFERENCES es_streams(aggregate_kind, aggregate_id);`
  - Optional safety: `ALTER TABLE es_streams ADD CONSTRAINT es_streams_last_position_nonneg CHECK (last_position IS NULL OR last_position >= 0);`
- Provide a documented “repair” SQL snippet for operators:
  - `UPDATE es_streams s SET last_position = e.max_pos FROM (SELECT aggregate_kind, aggregate_id, MAX(position) AS max_pos FROM es_events GROUP BY 1,2) e WHERE s.aggregate_kind = e.aggregate_kind AND s.aggregate_id = e.aggregate_id;`

## Testing gaps

There are currently no tests in `sourcery-postgres/`.

Minimum recommended coverage (ideally in `sourcery-postgres/tests/` behind a feature flag):

- `migrate` is idempotent (can run twice).
- `append_expecting_new` conflicts when stream already exists with events.
- `append` with expected version conflicts correctly under concurrent writers.
- `load_events` ordering is strictly by `position` across multiple filters.
- `after_position` filtering works (including boundary conditions).

Proposed solution (sketch):

- Add `sourcery-postgres/tests/postgres_store.rs` using `#[tokio::test]`.
- Read `DATABASE_URL` from env and `return`/`ignore` if missing (or gate behind a `postgres-test` feature).
- Use a unique schema per test run (e.g. `CREATE SCHEMA es_test_<uuid>` and set `search_path`) or unique table prefixes to keep tests isolated without requiring a dedicated database instance.

## Notes on compatibility

- The Postgres store is intentionally tied to Postgres-native concepts: `aggregate_id` is a `UUID` and `Position` is `i64` (`BIGSERIAL` / `BIGINT`). If you need other ID types later, prefer adding a separate store/schema rather than complicating the initial backend.
