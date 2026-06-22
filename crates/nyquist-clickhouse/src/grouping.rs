/// DDL helpers for the narrow (tall) ClickHouse schema.
///
/// Every metric, regardless of sampler, goes into a single `samples` table.
/// Labels are stored in a Map column so the schema never needs migration when
/// new labels are added (new interfaces, queues, CPUs, etc.).
///
/// Schema:
///   ts    DateTime64(3)       – unix epoch milliseconds
///   name  LowCardinality(String) – metric name with '/' replaced by '_'
///   tags  Map(LowCardinality(String), String) – all labels
///   raw   UInt64              – latest raw counter / gauge value
///   p50   UInt64              – 50th percentile over the sliding window
///   p90   UInt64              – 90th percentile
///   p99   UInt64              – 99th percentile
///   p999  UInt64              – 99.9th percentile

pub const TABLE: &str = "samples";

pub fn create_db_sql(db: &str) -> String {
    format!("CREATE DATABASE IF NOT EXISTS {db}")
}

pub fn create_table_sql(db: &str) -> String {
    format!(
        "CREATE TABLE IF NOT EXISTS {db}.{TABLE} (\
            ts   DateTime64(3, 'UTC'),\
            name LowCardinality(String),\
            tags Map(LowCardinality(String), String),\
            raw  UInt64,\
            p50  UInt64,\
            p90  UInt64,\
            p99  UInt64,\
            p999 UInt64\
        ) ENGINE = MergeTree()\
        PARTITION BY toDate(ts)\
        ORDER BY (name, tags, ts)\
        TTL toDate(ts) + INTERVAL 30 DAY"
    )
}

pub fn create_config_table_sql(db: &str) -> String {
    format!(
        "CREATE TABLE IF NOT EXISTS {db}.config_snapshots (\
            ts       DateTime64(3, 'UTC'),\
            hostname LowCardinality(String),\
            snapshot String\
        ) ENGINE = ReplacingMergeTree(ts)\
        ORDER BY (hostname, ts)"
    )
}

/// Convert a nyquist metric name (`cpu/usage/user`) to a ClickHouse-safe name.
pub fn metric_name(nyquist_name: &str) -> String {
    nyquist_name.replace('/', "_")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metric_name_replaces_slashes() {
        assert_eq!(metric_name("cpu/usage/user"), "cpu_usage_user");
        assert_eq!(metric_name("nic/queue/rx_bytes"), "nic_queue_rx_bytes");
        assert_eq!(metric_name("no_slash"), "no_slash");
    }

    #[test]
    fn ddl_contains_expected_columns() {
        let ddl = create_table_sql("nyquist");
        assert!(ddl.contains("nyquist.samples"));
        assert!(ddl.contains("DateTime64(3"));
        assert!(ddl.contains("Map(LowCardinality(String), String)"));
        assert!(ddl.contains("p999 UInt64"));
        assert!(ddl.contains("MergeTree()"));
        assert!(ddl.contains("ORDER BY (name, tags, ts)"));
    }
}
