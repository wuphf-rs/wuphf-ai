//! App-state resyncs: fetching a collection's full snapshot again, so the
//! connector sees state that synced before it read it.
//!
//! The library keeps each app-state collection's version and afterwards asks
//! only for patches, so a contact saved before this connector stored saved
//! names, or a chat pinned before it synced chat state, never reaches it
//! again. The connector therefore records, in its own `wuphf_app_state_seeds`
//! table, which collections it has seen whole at which [`SEEDS`] epoch. A link
//! made by this connector seeds them all (its first sync is a full one). On the
//! first connect of a session, any collection not seeded at its epoch is
//! resynced; `--resync-app-state` resyncs every collection once.
//!
//! A snapshot's events are reported as snapshots, so the core takes them as
//! current state: titles rename threads, but no notes are posted and no chat
//! state crosses to the mirrors.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use diesel::prelude::*;
use diesel::sql_types::{BigInt, Integer, Text};
use diesel::sqlite::SqliteConnection;
use tracing::{info, warn};
use whatsapp_rust::client::Client;
use whatsapp_rust::sync_task::MajorSyncTask;
use whatsapp_rust::wacore::appstate::hash::HashState;
use whatsapp_rust::wacore::appstate::patch_decode::WAPatchName;
use whatsapp_rust::wacore::store::error::StoreError;
use whatsapp_rust_sqlite_storage::SharedSqlite;

use crate::contacts::{database, now};

/// The collections the connector reads, each at the epoch its handlers are
/// at. Bump a collection's epoch when the connector starts reading something
/// new from it, so sessions seeded before that resync it once.
const SEEDS: &[(WAPatchName, i32)] = &[
    // Saved contact names (`contact` mutations).
    (WAPatchName::CriticalUnblockLow, 1),
    // Mute.
    (WAPatchName::RegularHigh, 1),
    // Pin and archive.
    (WAPatchName::RegularLow, 1),
];

/// Every collection, for a forced resync.
const ALL: [WAPatchName; 5] = [
    WAPatchName::CriticalBlock,
    WAPatchName::CriticalUnblockLow,
    WAPatchName::RegularHigh,
    WAPatchName::Regular,
    WAPatchName::RegularLow,
];

/// How long the first connect may take to drain offline messages and history
/// before resyncing anyway.
const STARTUP_SYNC_WAIT: Duration = Duration::from_secs(300);

const CREATE_TABLE: &str = "CREATE TABLE IF NOT EXISTS wuphf_app_state_seeds (
    collection TEXT PRIMARY KEY NOT NULL,
    epoch INTEGER NOT NULL,
    seeded_at INTEGER NOT NULL
)";

/// One session's resyncs, over its store's shared pool. Cheap to clone.
#[derive(Clone)]
pub struct Resync {
    shared: SharedSqlite,
    /// Resync every collection, not only the unseeded ones.
    force_all: bool,
    /// Set on the session's first connect: later reconnects resync nothing.
    started: Arc<AtomicBool>,
}

impl Resync {
    /// Create the table when it does not exist yet.
    pub async fn open(shared: SharedSqlite, force_all: bool) -> Result<Self, StoreError> {
        shared
            .run(|conn| create_table(conn).map_err(database))
            .await?;
        Ok(Self {
            shared,
            force_all,
            started: Arc::new(AtomicBool::new(false)),
        })
    }

    /// The device was just linked: its first sync is a full one, read by this
    /// connector's handlers, so every collection it reads is seeded.
    pub async fn linked(&self) {
        let result = self
            .shared
            .run(|conn| {
                let at = now();
                for &(collection, epoch) in SEEDS {
                    record(conn, collection.as_str(), epoch, at).map_err(database)?;
                }
                Ok(())
            })
            .await;
        if let Err(error) = result {
            warn!(%error, "could not record the WhatsApp app-state seeds of a new link");
        }
    }

    /// On a connect: the session's first one starts its resyncs in the
    /// background, once the startup sync has settled.
    pub fn connected(&self, client: Arc<Client>) {
        if self.started.swap(true, Ordering::SeqCst) {
            return;
        }
        let resync = self.clone();
        tokio::spawn(async move { resync.run(&client).await });
    }

    async fn run(&self, client: &Arc<Client>) {
        if let Err(error) = client.wait_for_startup_sync(STARTUP_SYNC_WAIT).await {
            warn!(%error, "resyncing WhatsApp app state before the startup sync settled");
        }
        let seeded = match self.shared.run(|conn| seeded(conn).map_err(database)).await {
            Ok(seeded) => seeded,
            Err(error) => {
                warn!(%error, "could not read the WhatsApp app-state seeds; resyncing nothing");
                return;
            }
        };
        for collection in due(&seeded, self.force_all) {
            if !resync(client, collection).await {
                continue;
            }
            let Some(epoch) = epoch(collection) else {
                continue;
            };
            let result = self
                .shared
                .run(move |conn| record(conn, collection.as_str(), epoch, now()).map_err(database))
                .await;
            if let Err(error) = result {
                warn!(%error, collection = collection.as_str(), "could not record a WhatsApp app-state seed");
            }
        }
    }
}

/// Fetch `collection`'s full snapshot again. Its stored version is reset
/// first, so should this fetch fail the library fetches the snapshot itself at
/// its next sync of it; a version above 0 afterwards means it was applied.
async fn resync(client: &Arc<Client>, collection: WAPatchName) -> bool {
    let name = collection.as_str();
    let backend = client.persistence_manager().backend();
    if let Err(error) = backend.set_version(name, HashState::default()).await {
        warn!(%error, collection = name, "could not reset a WhatsApp app-state collection");
        return false;
    }
    client
        .process_sync_task(MajorSyncTask::AppStateSync {
            name: collection,
            full_sync: true,
        })
        .await;
    match backend.get_version(name).await {
        Ok(state) if state.version > 0 => {
            info!(
                collection = name,
                version = state.version,
                "resynced WhatsApp app state"
            );
            true
        }
        Ok(_) => {
            warn!(
                collection = name,
                "a WhatsApp app-state resync did not complete; the library fetches it at its next sync"
            );
            false
        }
        Err(error) => {
            warn!(%error, collection = name, "could not read a resynced WhatsApp app-state version");
            false
        }
    }
}

/// The collections to resync: every one when forced, else each one the
/// connector reads that `seeded` (collection, epoch) does not hold at its
/// epoch.
fn due(seeded: &[(String, i32)], force_all: bool) -> Vec<WAPatchName> {
    if force_all {
        return ALL.to_vec();
    }
    SEEDS
        .iter()
        .filter(|&&(collection, epoch)| {
            !seeded
                .iter()
                .any(|(name, at)| name == collection.as_str() && *at >= epoch)
        })
        .map(|&(collection, _)| collection)
        .collect()
}

/// The epoch the connector reads `collection` at, if it reads it.
fn epoch(collection: WAPatchName) -> Option<i32> {
    SEEDS
        .iter()
        .find(|(seeded, _)| *seeded == collection)
        .map(|&(_, epoch)| epoch)
}

#[derive(QueryableByName)]
struct Seed {
    #[diesel(sql_type = Text)]
    collection: String,
    #[diesel(sql_type = Integer)]
    epoch: i32,
}

fn create_table(conn: &mut SqliteConnection) -> QueryResult<()> {
    diesel::sql_query(CREATE_TABLE).execute(conn).map(|_| ())
}

fn seeded(conn: &mut SqliteConnection) -> QueryResult<Vec<(String, i32)>> {
    Ok(
        diesel::sql_query("SELECT collection, epoch FROM wuphf_app_state_seeds")
            .load::<Seed>(conn)?
            .into_iter()
            .map(|seed| (seed.collection, seed.epoch))
            .collect(),
    )
}

fn record(conn: &mut SqliteConnection, collection: &str, epoch: i32, at: i64) -> QueryResult<()> {
    diesel::sql_query(
        "INSERT INTO wuphf_app_state_seeds (collection, epoch, seeded_at) VALUES (?, ?, ?)
         ON CONFLICT (collection) DO UPDATE SET epoch = excluded.epoch, seeded_at = excluded.seeded_at",
    )
    .bind::<Text, _>(collection)
    .bind::<Integer, _>(epoch)
    .bind::<BigInt, _>(at)
    .execute(conn)
    .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory() -> SqliteConnection {
        let mut conn = SqliteConnection::establish(":memory:").unwrap();
        create_table(&mut conn).unwrap();
        // Created twice: startup runs it on every start.
        create_table(&mut conn).unwrap();
        conn
    }

    #[test]
    fn a_session_never_seeded_resyncs_what_the_connector_reads() {
        assert_eq!(
            due(&[], false),
            [
                WAPatchName::CriticalUnblockLow,
                WAPatchName::RegularHigh,
                WAPatchName::RegularLow,
            ]
        );
    }

    #[test]
    fn a_seeded_collection_resyncs_only_after_its_epoch_moves() {
        let seeded = vec![
            ("critical_unblock_low".to_owned(), 1),
            ("regular_high".to_owned(), 1),
            // Seeded before the connector read pin and archive.
            ("regular_low".to_owned(), 0),
        ];
        assert_eq!(due(&seeded, false), [WAPatchName::RegularLow]);
    }

    #[test]
    fn a_forced_resync_takes_every_collection() {
        let seeded: Vec<_> = SEEDS
            .iter()
            .map(|&(collection, epoch)| (collection.as_str().to_owned(), epoch))
            .collect();
        assert!(due(&seeded, false).is_empty());
        assert_eq!(due(&seeded, true), ALL);
    }

    #[test]
    fn seeds_are_recorded_and_raised() {
        let mut conn = memory();
        assert!(seeded(&mut conn).unwrap().is_empty());
        record(&mut conn, "regular_low", 0, 5).unwrap();
        record(&mut conn, "regular_low", 1, 9).unwrap();
        record(&mut conn, "regular_high", 1, 9).unwrap();
        let mut found = seeded(&mut conn).unwrap();
        found.sort();
        assert_eq!(
            found,
            [
                ("regular_high".to_owned(), 1),
                ("regular_low".to_owned(), 1)
            ]
        );
        assert_eq!(epoch(WAPatchName::RegularLow), Some(1));
        assert_eq!(epoch(WAPatchName::Regular), None);
    }
}
