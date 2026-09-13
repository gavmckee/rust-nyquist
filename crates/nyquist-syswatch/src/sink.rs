use clickhouse::Client;
use crate::snapshot::Change;

pub struct Sink {
    client:      Client,
    base: Client,
    database:    String,
    initialized: bool,
}

impl Sink {
    pub fn new(url: &str, database: &str, username: &str, password: &str) -> Self {
        let client = Client::default()
            .with_url(url)
            .with_database(database)
            .with_user(username)
            .with_password(password);
        Sink { base: Client::default().with_url(url).with_user(username).with_password(password), client, database: database.to_string(), initialized: false }
    }

    pub async fn ensure_tables(&mut self) -> anyhow::Result<()> {
        if self.initialized { return Ok(()); }

        self.base.query(&format!("CREATE DATABASE IF NOT EXISTS {}", self.database))
            .execute().await?;

        // Use the same sysconfig_changes table as the polling ConfigWatcher so
        // both event sources appear together in Grafana annotation queries.
        self.client.query(&format!(
            "CREATE TABLE IF NOT EXISTS {db}.sysconfig_changes (\
                ts        DateTime64(3, 'UTC'),\
                hostname  LowCardinality(String),\
                key       LowCardinality(String),\
                old_value String,\
                new_value String,\
                note      String\
            ) ENGINE = MergeTree()\
            PARTITION BY toDate(ts)\
            ORDER BY (ts, key)",
            db = self.database,
        )).execute().await?;

        self.initialized = true;
        Ok(())
    }

    pub async fn insert_changes(
        &self,
        changes: &[(i64, Change)],
    ) -> anyhow::Result<()> {
        if changes.is_empty() { return Ok(()); }

        let host = hostname();
        let mut sql = format!(
            "INSERT INTO {}.sysconfig_changes \
             (ts, hostname, key, old_value, new_value, note) VALUES ",
            self.database
        );

        for (i, (ts_ms, c)) in changes.iter().enumerate() {
            if i > 0 { sql.push(','); }
            let note = format!(
                "{}: {} → {}  (pid={}, comm={})",
                c.key, c.old_value, c.new_value, c.pid, c.comm
            );
            sql.push_str(&format!(
                "(fromUnixTimestamp64Milli({ts_ms}, 'UTC'), '{}', '{}', '{}', '{}', '{}')",
                esc(&host),
                esc(&c.key),
                esc(&c.old_value),
                esc(&c.new_value),
                esc(&note),
            ));
        }

        self.client.query(&sql).execute().await?;
        Ok(())
    }
}

fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}
