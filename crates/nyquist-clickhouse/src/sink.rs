use std::time::{Duration, Instant, UNIX_EPOCH};
use async_trait::async_trait;
use clickhouse::Client;
use nyquist_core::sink::{Sink, SinkError};
use nyquist_core::snapshot::RegistrySnapshot;
use crate::grouping::{create_db_sql, create_table_sql, TABLE};
use crate::inserter::NarrowRow;

pub struct ClickHouseSink {
    /// Base client (no database) — used only for CREATE DATABASE.
    base:            Client,
    /// Database-scoped client — used for all table operations.
    client:          Client,
    database:        String,
    insert_interval: Duration,
    last_insert:     Instant,
    initialized:     bool,
}

impl ClickHouseSink {
    pub fn new(
        url:             &str,
        database:        &str,
        username:        &str,
        password:        &str,
        insert_interval: Duration,
    ) -> Self {
        let base = Client::default()
            .with_url(url)
            .with_user(username)
            .with_password(password);
        let client = Client::default()
            .with_url(url)
            .with_database(database)
            .with_user(username)
            .with_password(password);
        ClickHouseSink {
            base,
            client,
            database: database.to_string(),
            insert_interval,
            last_insert:  Instant::now(),
            initialized:  false,
        }
    }

    async fn ensure_schema(&mut self) -> Result<(), SinkError> {
        if self.initialized { return Ok(()); }
        self.base.query(&create_db_sql(&self.database)).execute().await?;
        self.client.query(&create_table_sql(&self.database)).execute().await?;
        self.initialized = true;
        tracing::info!(database = %self.database, "ClickHouse schema ready");
        Ok(())
    }

    async fn insert_samples(&self, rows: &[NarrowRow]) -> Result<(), SinkError> {
        if rows.is_empty() { return Ok(()); }
        let mut sql = format!(
            "INSERT INTO {}.{TABLE} (ts, name, tags, raw, p50, p90, p99, p999) VALUES ",
            self.database
        );
        for (i, row) in rows.iter().enumerate() {
            if i > 0 { sql.push(','); }
            row.append_to(&mut sql);
        }
        self.client.query(&sql).execute().await?;
        Ok(())
    }
}

#[async_trait]
impl Sink for ClickHouseSink {
    async fn export(&mut self, snapshot: &RegistrySnapshot) -> Result<(), SinkError> {
        if let Err(e) = self.ensure_schema().await {
            tracing::warn!(error = %e, "clickhouse: schema init failed, skipping export");
            return Err(e);
        }
        if self.last_insert.elapsed() < self.insert_interval {
            return Ok(());
        }
        self.last_insert = Instant::now();

        let ts_ms = snapshot.captured
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;

        let rows: Vec<NarrowRow> = snapshot.metrics.iter()
            .map(|m| NarrowRow::from_snapshot(ts_ms, m))
            .collect();

        match self.insert_samples(&rows).await {
            Ok(()) => {
                tracing::debug!(rows = rows.len(), ts_ms, "clickhouse: batch inserted");
                Ok(())
            }
            Err(e) => {
                tracing::warn!(error = %e, rows = rows.len(), "clickhouse: insert failed");
                Err(e)
            }
        }
    }
}
