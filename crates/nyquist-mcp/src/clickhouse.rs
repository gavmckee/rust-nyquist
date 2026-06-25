//! Thin async ClickHouse HTTP client for the nyquist `samples` table.
//! Connection params come from env (NYQUIST_CH_*) with dev-stack defaults.

use anyhow::{Context, Result};

#[derive(Clone)]
pub struct ClickHouse {
    client: reqwest::Client,
    url:    String,
    db:     String,
    user:   String,
    pass:   String,
}

impl ClickHouse {
    pub fn from_env() -> Self {
        let env = |k: &str, d: &str| std::env::var(k).unwrap_or_else(|_| d.to_string());
        ClickHouse {
            client: reqwest::Client::new(),
            url:    env("NYQUIST_CH_URL", "http://localhost:18123"),
            db:     env("NYQUIST_CH_DB", "nyquist_live"),
            user:   env("NYQUIST_CH_USER", "default"),
            pass:   env("NYQUIST_CH_PASS", "nyquist"),
        }
    }

    /// Run a SQL statement and return the raw response body (caller chooses FORMAT).
    pub async fn query(&self, sql: &str) -> Result<String> {
        let resp = self
            .client
            .post(format!("{}/?database={}", self.url, self.db))
            .basic_auth(&self.user, Some(&self.pass))
            .body(sql.to_string())
            .send()
            .await
            .context("clickhouse request failed (is the dev stack up on :18123?)")?;
        let status = resp.status();
        let text = resp.text().await.context("reading clickhouse body")?;
        if !status.is_success() {
            anyhow::bail!("clickhouse {}: {}", status, text.trim());
        }
        Ok(text)
    }

    pub fn endpoint(&self) -> String {
        format!("{} db={}", self.url, self.db)
    }
}
