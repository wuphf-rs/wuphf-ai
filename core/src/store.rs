//! SQLite setup, migrations, and durable configured-topology initialization.

use crate::config::Config;
use sqlx::{
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
    SqlitePool,
};
use std::{str::FromStr, time::Duration};

#[derive(Clone)]
/// Initialized SQLite persistence handle shared by repository operations.
pub struct Store {
    /// SQLx connection pool used for durable core transactions.
    pub(crate) pool: SqlitePool,
}

impl Store {
    /// Opens configured SQLite database, applies migrations, and persists topology references.
    pub async fn open(config: &Config) -> Result<Self, sqlx::Error> {
        let options = SqliteConnectOptions::from_str(&config.database.url)?
            .create_if_missing(true)
            .foreign_keys(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Full)
            .busy_timeout(Duration::from_secs(5));
        let pool = SqlitePoolOptions::new()
            // Sequence allocation and command claiming rely on one serialized writer.
            .max_connections(1)
            .connect_with(options)
            .await?;
        sqlx::migrate!("../migrations").run(&pool).await?;
        let store = Self { pool };
        store.persist_topology(config).await?;
        let migration_version: i64 =
            sqlx::query_scalar("SELECT COALESCE(MAX(version), 0) FROM _sqlx_migrations")
                .fetch_one(&store.pool)
                .await?;
        tracing::info!(
            migration_version,
            connector_count = config.connectors.len(),
            endpoint_count = config.endpoints.len(),
            replication_group_count = config.replication_groups.len(),
            "sqlite durable state ready"
        );
        Ok(store)
    }

    /// Stores validated configured connectors, endpoints, and group memberships transactionally.
    async fn persist_topology(&self, config: &Config) -> Result<(), sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        for connector in &config.connectors {
            sqlx::query("INSERT OR IGNORE INTO configured_connectors(id) VALUES (?)")
                .bind(&connector.id)
                .execute(&mut *tx)
                .await?;
        }
        for endpoint in &config.endpoints {
            sqlx::query(
                "INSERT INTO configured_endpoints(id, connector_id, address) VALUES (?, ?, ?)
                 ON CONFLICT(id) DO UPDATE SET connector_id = excluded.connector_id, address = excluded.address",
            )
            .bind(&endpoint.id)
            .bind(&endpoint.connector_id)
            .bind(&endpoint.address)
            .execute(&mut *tx)
            .await?;
        }
        for group in &config.replication_groups {
            sqlx::query("INSERT OR IGNORE INTO configured_replication_groups(id) VALUES (?)")
                .bind(&group.id)
                .execute(&mut *tx)
                .await?;
            for endpoint_id in &group.endpoints {
                sqlx::query(
                    "INSERT OR IGNORE INTO configured_group_endpoints(group_id, endpoint_id) VALUES (?, ?)",
                )
                .bind(&group.id)
                .bind(endpoint_id)
                .execute(&mut *tx)
                .await?;
            }
        }
        tx.commit().await
    }
}

#[cfg(test)]
/// Unit tests for durable SQLite connection invariants.
mod tests {
    use super::*;
    use std::borrow::Cow;
    use tempfile::TempDir;

    /// Creates valid topology configured over one temporary SQLite file.
    fn config(temp: &TempDir) -> Config {
        toml::from_str(&format!(
            r#"
                [server]
                listen = "127.0.0.1:1"
                [database]
                url = "sqlite://{}"
                [[connectors]]
                id = "a"
                token = "a"
                [[connectors]]
                id = "b"
                token = "b"
                [[endpoints]]
                id = "a-room"
                connector_id = "a"
                address = "a"
                [[endpoints]]
                id = "b-room"
                connector_id = "b"
                address = "b"
                [[replication_groups]]
                id = "rooms"
                endpoints = ["a-room", "b-room"]
            "#,
            temp.path().join("store.sqlite").display()
        ))
        .unwrap()
    }

    #[tokio::test]
    /// Applies durable connection safety settings on initialized store.
    async fn enables_sqlite_durability_pragmas() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(&config(&temp)).await.unwrap();

        let foreign_keys: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
            .fetch_one(&store.pool)
            .await
            .unwrap();
        let journal_mode: String = sqlx::query_scalar("PRAGMA journal_mode")
            .fetch_one(&store.pool)
            .await
            .unwrap();
        let synchronous: i64 = sqlx::query_scalar("PRAGMA synchronous")
            .fetch_one(&store.pool)
            .await
            .unwrap();
        let busy_timeout: i64 = sqlx::query_scalar("PRAGMA busy_timeout")
            .fetch_one(&store.pool)
            .await
            .unwrap();

        assert_eq!(foreign_keys, 1);
        assert_eq!(journal_mode, "wal");
        assert_eq!(synchronous, 2);
        assert_eq!(busy_timeout, 5_000);
    }

    #[tokio::test]
    /// Rejects topology writes that violate configured foreign-key ownership.
    async fn enforces_foreign_keys_after_migration() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(&config(&temp)).await.unwrap();
        let result = sqlx::query(
            "INSERT INTO configured_endpoints(id, connector_id, address) VALUES (?, ?, ?)",
        )
        .bind("invalid")
        .bind("missing")
        .bind("invalid")
        .execute(&store.pool)
        .await;

        assert!(result.is_err());
    }

    #[tokio::test]
    /// Upgrades an existing v2 database while retaining durable dispatch rows.
    async fn upgrades_previous_schema_and_retains_dispatch_state() {
        let temp = tempfile::tempdir().unwrap();
        let config = config(&temp);
        let options = SqliteConnectOptions::from_str(&config.database.url)
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        let current = sqlx::migrate!("../migrations");
        let previous = sqlx::migrate::Migrator {
            migrations: Cow::Owned(current.iter().take(2).cloned().collect()),
            ..sqlx::migrate::Migrator::DEFAULT
        };
        previous.run(&pool).await.unwrap();
        sqlx::query("INSERT INTO configured_connectors(id) VALUES ('b')")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO configured_endpoints(id, connector_id, address)
             VALUES ('b-room', 'b', 'b')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO logical_messages
             (id, origin_endpoint_id, sender_id, sender_display_name, sent_at_unix_ms, text, attachments_json)
             VALUES ('message', 'b-room', 'sender', 'Sender', 1, 'text', '[]')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO dispatch_commands
             (id, logical_message_id, target_endpoint_id, target_connector_id, endpoint_sequence, status, terminal_failure)
             VALUES ('dispatch', 'message', 'b-room', 'b', 1, 'failed', 1)",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool.close().await;

        let upgraded = Store::open(&config).await.unwrap();
        let version: i64 =
            sqlx::query_scalar("SELECT COALESCE(MAX(version), 0) FROM _sqlx_migrations")
                .fetch_one(&upgraded.pool)
                .await
                .unwrap();
        let retained: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM dispatch_commands WHERE id = 'dispatch'")
                .fetch_one(&upgraded.pool)
                .await
                .unwrap();
        let index_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type = 'index' AND name = 'dispatch_terminal_lookup'",
        )
        .fetch_one(&upgraded.pool)
        .await
        .unwrap();

        assert_eq!(version, 3);
        assert_eq!(retained, 1);
        assert_eq!(index_count, 1);
    }
}
