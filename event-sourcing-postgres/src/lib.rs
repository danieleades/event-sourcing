//! Postgres-backed event store implementation.
//!
//! This crate provides [`PostgresEventStore`], an implementation of
//! [`event_sourcing_core::store::EventStore`] for `PostgreSQL`.

use std::marker::PhantomData;

use event_sourcing_core::codec::Codec;
use event_sourcing_core::concurrency::{ConcurrencyConflict, ConcurrencyStrategy};
use event_sourcing_core::store::{
    AppendError, EventFilter, EventStore, LoadEventsResult, PersistableEvent, StoredEvent,
    Transaction,
};
use serde::{Serialize, de::DeserializeOwned};
use sqlx::{PgPool, Postgres, QueryBuilder, Row};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PostgresStoreError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("invalid position value from database: {0}")]
    InvalidPosition(i64),
    #[error("database did not return an inserted position")]
    MissingReturnedPosition,
}

/// A PostgreSQL-backed [`EventStore`].
///
/// Defaults are intentionally conservative:
/// - Positions are global and monotonic (`i64`, backed by `BIGSERIAL`).
/// - Metadata is stored as `jsonb` (`M: Serialize + DeserializeOwned`).
#[derive(Clone)]
pub struct PostgresEventStore<C, M> {
    pool: PgPool,
    codec: C,
    _phantom: PhantomData<M>,
}

impl<C, M> PostgresEventStore<C, M>
where
    C: Sync,
    M: Sync,
{
    #[must_use]
    pub const fn new(pool: PgPool, codec: C) -> Self {
        Self {
            pool,
            codec,
            _phantom: PhantomData,
        }
    }

    /// Apply the initial schema (idempotent).
    ///
    /// This uses `CREATE TABLE IF NOT EXISTS` style DDL so it can be run on startup.
    ///
    /// # Errors
    ///
    /// Returns a `sqlx::Error` if any of the schema creation queries fail.
    #[tracing::instrument(skip(self))]
    pub async fn migrate(&self) -> Result<(), sqlx::Error> {
        // Streams track per-aggregate last position for optimistic concurrency.
        sqlx::query(
            r"
            CREATE TABLE IF NOT EXISTS es_streams (
                aggregate_kind TEXT NOT NULL,
                aggregate_id   UUID NOT NULL,
                last_position  BIGINT NULL,
                PRIMARY KEY (aggregate_kind, aggregate_id)
            )
            ",
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r"
            CREATE TABLE IF NOT EXISTS es_events (
                position       BIGSERIAL PRIMARY KEY,
                aggregate_kind TEXT NOT NULL,
                aggregate_id   UUID NOT NULL,
                event_kind     TEXT NOT NULL,
                data           BYTEA NOT NULL,
                metadata       JSONB NOT NULL,
                created_at     TIMESTAMPTZ NOT NULL DEFAULT now()
            )
            ",
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r"CREATE INDEX IF NOT EXISTS es_events_by_kind_and_position ON es_events(event_kind, position)",
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r"CREATE INDEX IF NOT EXISTS es_events_by_stream_and_position ON es_events(aggregate_kind, aggregate_id, position)",
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }
}

impl<C, M> EventStore for PostgresEventStore<C, M>
where
    C: Codec + Clone + Send + Sync + 'static,
    M: Serialize + DeserializeOwned + Send + Sync + 'static,
{
    type Id = uuid::Uuid;
    type Position = i64;
    type Error = PostgresStoreError;
    type Codec = C;
    type Metadata = M;

    fn codec(&self) -> &Self::Codec {
        &self.codec
    }

    async fn stream_version<'a>(
        &'a self,
        aggregate_kind: &'a str,
        aggregate_id: &'a Self::Id,
    ) -> Result<Option<Self::Position>, Self::Error> {
        let result: Option<i64> = sqlx::query_scalar(
            r"SELECT last_position FROM es_streams WHERE aggregate_kind = $1 AND aggregate_id = $2",
        )
        .bind(aggregate_kind)
        .bind(aggregate_id)
        .fetch_optional(&self.pool)
        .await?
        .flatten();

        Ok(result)
    }

    fn begin<Conc: ConcurrencyStrategy>(
        &mut self,
        aggregate_kind: &str,
        aggregate_id: Self::Id,
        expected_version: Option<Self::Position>,
    ) -> Transaction<'_, Self, Conc> {
        Transaction::new(
            self,
            aggregate_kind.to_string(),
            aggregate_id,
            expected_version,
        )
    }

    #[tracing::instrument(
        skip(self, events),
        fields(
            aggregate_kind,
            aggregate_id = %aggregate_id,
            expected_version,
            events_len = events.len()
        )
    )]
    async fn append<'a>(
        &'a mut self,
        aggregate_kind: &'a str,
        aggregate_id: &'a Self::Id,
        expected_version: Option<Self::Position>,
        events: Vec<PersistableEvent<Self::Metadata>>,
    ) -> Result<(), AppendError<Self::Position, Self::Error>> {
        if events.is_empty() {
            return Ok(());
        }

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| AppendError::store(PostgresStoreError::Database(e)))?;

        sqlx::query(
            r"
                INSERT INTO es_streams (aggregate_kind, aggregate_id, last_position)
                VALUES ($1, $2, NULL)
                ON CONFLICT (aggregate_kind, aggregate_id) DO NOTHING
                ",
        )
        .bind(aggregate_kind)
        .bind(aggregate_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| AppendError::store(PostgresStoreError::Database(e)))?;

        let current: Option<i64> = sqlx::query_scalar(
            r"
                SELECT last_position
                FROM es_streams
                WHERE aggregate_kind = $1 AND aggregate_id = $2
                FOR UPDATE
                ",
        )
        .bind(aggregate_kind)
        .bind(aggregate_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| AppendError::store(PostgresStoreError::Database(e)))?;

        if let Some(expected) = expected_version
            && current != Some(expected)
        {
            return Err(AppendError::Conflict(ConcurrencyConflict {
                expected: Some(expected),
                actual: current,
            }));
        }

        let mut qb = QueryBuilder::<Postgres>::new(
            "INSERT INTO es_events (aggregate_kind, aggregate_id, event_kind, data, metadata) ",
        );
        qb.push_values(events, |mut b, event| {
            b.push_bind(aggregate_kind);
            b.push_bind(aggregate_id);
            b.push_bind(event.kind);
            b.push_bind(event.data);
            b.push_bind(sqlx::types::Json(event.metadata));
        });
        qb.push(" RETURNING position");

        let rows: Vec<i64> = qb
            .build_query_scalar()
            .fetch_all(&mut *tx)
            .await
            .map_err(|e| AppendError::store(PostgresStoreError::Database(e)))?;

        let last_position = rows
            .last()
            .ok_or_else(|| AppendError::store(PostgresStoreError::MissingReturnedPosition))?;

        sqlx::query(
            r"
                UPDATE es_streams
                SET last_position = $1
                WHERE aggregate_kind = $2 AND aggregate_id = $3
                ",
        )
        .bind(last_position)
        .bind(aggregate_kind)
        .bind(aggregate_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| AppendError::store(PostgresStoreError::Database(e)))?;

        tx.commit()
            .await
            .map_err(|e| AppendError::store(PostgresStoreError::Database(e)))?;

        Ok(())
    }

    #[tracing::instrument(skip(self, filters), fields(filters_len = filters.len()))]
    async fn load_events<'a>(
        &'a self,
        filters: &'a [EventFilter<Self::Id, Self::Position>],
    ) -> LoadEventsResult<Self::Id, Self::Position, Self::Metadata, Self::Error> {
        if filters.is_empty() {
            return Ok(Vec::new());
        }

        let mut qb = QueryBuilder::<Postgres>::new(
            "SELECT aggregate_kind, aggregate_id, event_kind, position, data, metadata FROM (",
        );

        for (i, filter) in filters.iter().enumerate() {
            if i > 0 {
                qb.push(" UNION ALL ");
            }

            qb.push("SELECT aggregate_kind, aggregate_id, event_kind, position, data, metadata FROM es_events WHERE event_kind = ").push_bind(&filter.event_kind);

            if let Some(kind) = &filter.aggregate_kind {
                qb.push(" AND aggregate_kind = ").push_bind(kind);
            }

            if let Some(id) = &filter.aggregate_id {
                qb.push(" AND aggregate_id = ").push_bind(id);
            }

            if let Some(after) = filter.after_position {
                if after < 0 {
                    return Err(PostgresStoreError::InvalidPosition(after));
                }
                qb.push(" AND position > ").push_bind(after);
            }
        }

        qb.push(") t ORDER BY position ASC");

        let rows = qb.build().fetch_all(&self.pool).await?;

        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let aggregate_kind: String = row.try_get("aggregate_kind")?;
            let aggregate_id: uuid::Uuid = row.try_get("aggregate_id")?;
            let event_kind: String = row.try_get("event_kind")?;
            let position: i64 = row.try_get("position")?;
            let data: Vec<u8> = row.try_get("data")?;
            let metadata: sqlx::types::Json<M> = row.try_get("metadata")?;

            out.push(StoredEvent {
                aggregate_kind,
                aggregate_id,
                kind: event_kind,
                position,
                data,
                metadata: metadata.0,
            });
        }

        Ok(out)
    }

    #[tracing::instrument(
        skip(self, events),
        fields(aggregate_kind, aggregate_id = %aggregate_id, events_len = events.len())
    )]
    async fn append_expecting_new<'a>(
        &'a mut self,
        aggregate_kind: &'a str,
        aggregate_id: &'a Self::Id,
        events: Vec<PersistableEvent<Self::Metadata>>,
    ) -> Result<(), AppendError<Self::Position, Self::Error>> {
        if events.is_empty() {
            return Ok(());
        }

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|e| AppendError::store(PostgresStoreError::Database(e)))?;

        sqlx::query(
            r"
                INSERT INTO es_streams (aggregate_kind, aggregate_id, last_position)
                VALUES ($1, $2, NULL)
                ON CONFLICT (aggregate_kind, aggregate_id) DO NOTHING
                ",
        )
        .bind(aggregate_kind)
        .bind(aggregate_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| AppendError::store(PostgresStoreError::Database(e)))?;

        let current: Option<i64> = sqlx::query_scalar(
            r"
                SELECT last_position
                FROM es_streams
                WHERE aggregate_kind = $1 AND aggregate_id = $2
                FOR UPDATE
                ",
        )
        .bind(aggregate_kind)
        .bind(aggregate_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| AppendError::store(PostgresStoreError::Database(e)))?;

        if let Some(actual) = current {
            return Err(AppendError::Conflict(ConcurrencyConflict {
                expected: None,
                actual: Some(actual),
            }));
        }

        let mut qb = QueryBuilder::<Postgres>::new(
            "INSERT INTO es_events (aggregate_kind, aggregate_id, event_kind, data, metadata) ",
        );
        qb.push_values(events, |mut b, event| {
            b.push_bind(aggregate_kind);
            b.push_bind(aggregate_id);
            b.push_bind(event.kind);
            b.push_bind(event.data);
            b.push_bind(sqlx::types::Json(event.metadata));
        });
        qb.push(" RETURNING position");

        let rows: Vec<i64> = qb
            .build_query_scalar()
            .fetch_all(&mut *tx)
            .await
            .map_err(|e| AppendError::store(PostgresStoreError::Database(e)))?;

        let last_position = rows
            .last()
            .ok_or_else(|| AppendError::store(PostgresStoreError::MissingReturnedPosition))?;

        sqlx::query(
            r"
                UPDATE es_streams
                SET last_position = $1
                WHERE aggregate_kind = $2 AND aggregate_id = $3
                ",
        )
        .bind(last_position)
        .bind(aggregate_kind)
        .bind(aggregate_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| AppendError::store(PostgresStoreError::Database(e)))?;

        tx.commit()
            .await
            .map_err(|e| AppendError::store(PostgresStoreError::Database(e)))?;

        Ok(())
    }
}
