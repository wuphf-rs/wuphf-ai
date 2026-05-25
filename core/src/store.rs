//! SQLite setup, migrations, and durable configured-topology initialization.

use crate::config::Config;
use sqlx::{
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
    SqlitePool,
};
use std::str::FromStr;

#[derive(Clone)]
/// Initialized SQLite persistence handle shared by repository operations.
pub struct Store {
    /// SQLx connection pool used for durable core transactions.
    pub(crate) pool: SqlitePool,
}

impl Store {
    /// Opens configured SQLite database, applies migrations, and persists topology references.
    pub async fn open(config: &Config) -> Result<Self, sqlx::Error> {
        let options = SqliteConnectOptions::from_str(&config.database.url)?.create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await?;
        sqlx::migrate!("../migrations").run(&pool).await?;
        let store = Self { pool };
        store.persist_topology(config).await?;
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
