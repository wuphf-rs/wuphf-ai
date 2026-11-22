//! The connector's own contact names: saved address-book names (full / first)
//! from app-state contact syncs and the push names people send with their
//! messages or that a history sync lists. whatsapp-rust stores neither for
//! other people, and its chat-store crate would store the whole message
//! history, so the connector keeps this small `wuphf_contacts` table itself.
//!
//! The table lives in the session store's SQLite file and is reached through
//! the store's shared pool ([`SharedSqlite`]), the library's documented path
//! for sibling tables: one pool and one write semaphore for the file, and no
//! second SQLite driver. It is named `wuphf_contacts` so a later switch to the
//! chat-store crate (which creates `contacts`) cannot collide. There is no
//! migration runner: a schema change needs one (see plan 046's maintenance
//! notes).
//!
//! The SQL lives in plain functions over a [`SqliteConnection`], so it is
//! tested against an in-memory database without a store.

use std::time::{SystemTime, UNIX_EPOCH};

use diesel::prelude::*;
use diesel::sql_types::{BigInt, Nullable, Text};
use diesel::sqlite::SqliteConnection;
use tracing::warn;
use whatsapp_rust::wacore::store::error::StoreError;
use whatsapp_rust_sqlite_storage::SharedSqlite;

const CREATE_TABLE: &str = "CREATE TABLE IF NOT EXISTS wuphf_contacts (
    jid TEXT PRIMARY KEY NOT NULL,
    full_name TEXT,
    first_name TEXT,
    push_name TEXT,
    updated_at INTEGER NOT NULL
)";

/// What the connector knows about a person's names. Every field is trimmed
/// and absent rather than empty.
#[derive(Clone, Debug, Default, PartialEq, Eq, QueryableByName)]
pub struct ContactNames {
    #[diesel(sql_type = Nullable<Text>)]
    pub full_name: Option<String>,
    #[diesel(sql_type = Nullable<Text>)]
    pub first_name: Option<String>,
    #[diesel(sql_type = Nullable<Text>)]
    pub push_name: Option<String>,
}

impl ContactNames {
    /// Whether a name a title can show is known (a saved full name or a push
    /// name; a first name alone is not shown).
    pub fn is_named(&self) -> bool {
        self.full_name.is_some() || self.push_name.is_some()
    }

    /// Fill the fields this one lacks from `other`.
    fn fill_from(&mut self, other: ContactNames) {
        self.full_name = self.full_name.take().or(other.full_name);
        self.first_name = self.first_name.take().or(other.first_name);
        self.push_name = self.push_name.take().or(other.push_name);
    }
}

/// The contacts table, through the session store's shared pool. Failures are
/// logged and read as "nothing known": a name is never worth failing a
/// message over.
pub struct Contacts {
    shared: SharedSqlite,
}

impl Contacts {
    /// Create the table when it does not exist yet.
    pub async fn open(shared: SharedSqlite) -> Result<Self, StoreError> {
        shared
            .run(|conn| create_table(conn).map_err(database))
            .await?;
        Ok(Self { shared })
    }

    /// Record the push name `jid` sent. An empty name records nothing.
    pub async fn upsert_push_name(&self, jid: String, name: &str) {
        let Some(name) = clean(Some(name)) else {
            return;
        };
        let result = self
            .shared
            .run(move |conn| upsert_push_name(conn, &jid, &name, now()).map_err(database))
            .await;
        if let Err(error) = result {
            warn!(%error, "could not store a WhatsApp push name");
        }
    }

    /// Record a saved contact's names under every form given (its JID and,
    /// when the sync names them, its phone and LID forms).
    pub async fn upsert_contact_name(
        &self,
        jids: Vec<String>,
        full: Option<&str>,
        first: Option<&str>,
    ) {
        let (full, first) = (clean(full), clean(first));
        let result = self
            .shared
            .run(move |conn| {
                conn.transaction::<_, diesel::result::Error, _>(|conn| {
                    for jid in &jids {
                        upsert_contact_name(conn, jid, full.as_deref(), first.as_deref(), now())?;
                    }
                    Ok(())
                })
                .map_err(database)
            })
            .await;
        if let Err(error) = result {
            warn!(%error, "could not store a WhatsApp contact name");
        }
    }

    /// The names known under any of `forms` (a JID and its other phone or LID
    /// form), the first form's names winning field by field.
    pub async fn get(&self, forms: Vec<String>) -> ContactNames {
        let result = self
            .shared
            .run(move |conn| lookup(conn, &forms).map_err(database))
            .await;
        result.unwrap_or_else(|error| {
            warn!(%error, "could not read WhatsApp contact names");
            ContactNames::default()
        })
    }

    /// Run `f` on the contacts database (a history sync stores its push names
    /// and reads its titles in one go).
    pub async fn run<F, T>(&self, f: F) -> Result<T, StoreError>
    where
        F: FnOnce(&mut SqliteConnection) -> QueryResult<T> + Send + 'static,
        T: Send + 'static,
    {
        self.shared.run(move |conn| f(conn).map_err(database)).await
    }
}

pub(crate) fn database(error: diesel::result::Error) -> StoreError {
    StoreError::Database(Box::new(error))
}

/// Now, in Unix seconds.
pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or_default()
}

/// A trimmed name, or `None` when there is nothing in it.
fn clean(name: Option<&str>) -> Option<String> {
    let name = name?.trim();
    (!name.is_empty()).then(|| name.to_owned())
}

pub fn create_table(conn: &mut SqliteConnection) -> QueryResult<()> {
    diesel::sql_query(CREATE_TABLE).execute(conn).map(|_| ())
}

/// The row stored under exactly `jid`.
fn select(conn: &mut SqliteConnection, jid: &str) -> QueryResult<Option<ContactNames>> {
    diesel::sql_query("SELECT full_name, first_name, push_name FROM wuphf_contacts WHERE jid = ?")
        .bind::<Text, _>(jid)
        .get_result::<ContactNames>(conn)
        .optional()
}

/// The names under any of `forms`, merged in order.
pub fn lookup(conn: &mut SqliteConnection, forms: &[String]) -> QueryResult<ContactNames> {
    let mut names = ContactNames::default();
    for form in forms {
        if let Some(found) = select(conn, form)? {
            names.fill_from(found);
        }
    }
    Ok(names)
}

/// Store `name` as `jid`'s push name. Reads first and writes only a changed
/// value, so a push name on every message costs one indexed read. Returns
/// whether a row was written.
pub fn upsert_push_name(
    conn: &mut SqliteConnection,
    jid: &str,
    name: &str,
    at: i64,
) -> QueryResult<bool> {
    if select(conn, jid)?.is_some_and(|row| row.push_name.as_deref() == Some(name)) {
        return Ok(false);
    }
    diesel::sql_query(
        "INSERT INTO wuphf_contacts (jid, push_name, updated_at) VALUES (?, ?, ?)
         ON CONFLICT(jid) DO UPDATE SET
             push_name = excluded.push_name,
             updated_at = excluded.updated_at",
    )
    .bind::<Text, _>(jid)
    .bind::<Text, _>(name)
    .bind::<BigInt, _>(at)
    .execute(conn)?;
    Ok(true)
}

/// Store a list of push names (empty ones skipped) in one transaction.
pub fn store_push_names(
    conn: &mut SqliteConnection,
    entries: &[(String, String)],
    at: i64,
) -> QueryResult<()> {
    conn.transaction(|conn| {
        for (jid, name) in entries {
            if let Some(name) = clean(Some(name)) {
                upsert_push_name(conn, jid, &name, at)?;
            }
        }
        Ok(())
    })
}

/// Store a saved contact's full and first names (absent ones clear the
/// stored value: the sync action is the contact's current state). Writes only
/// a change. Returns whether a row was written.
pub fn upsert_contact_name(
    conn: &mut SqliteConnection,
    jid: &str,
    full: Option<&str>,
    first: Option<&str>,
    at: i64,
) -> QueryResult<bool> {
    let stored = select(conn, jid)?;
    let unchanged = match &stored {
        Some(row) => row.full_name.as_deref() == full && row.first_name.as_deref() == first,
        // Nothing to clear on a row that does not exist.
        None => full.is_none() && first.is_none(),
    };
    if unchanged {
        return Ok(false);
    }
    diesel::sql_query(
        "INSERT INTO wuphf_contacts (jid, full_name, first_name, updated_at) VALUES (?, ?, ?, ?)
         ON CONFLICT(jid) DO UPDATE SET
             full_name = excluded.full_name,
             first_name = excluded.first_name,
             updated_at = excluded.updated_at",
    )
    .bind::<Text, _>(jid)
    .bind::<Nullable<Text>, _>(full)
    .bind::<Nullable<Text>, _>(first)
    .bind::<BigInt, _>(at)
    .execute(conn)?;
    Ok(true)
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub fn memory() -> SqliteConnection {
        let mut conn = SqliteConnection::establish(":memory:").unwrap();
        create_table(&mut conn).unwrap();
        // Created twice: startup runs it on every start.
        create_table(&mut conn).unwrap();
        conn
    }

    fn updated_at(conn: &mut SqliteConnection, jid: &str) -> i64 {
        #[derive(QueryableByName)]
        struct Row {
            #[diesel(sql_type = BigInt)]
            updated_at: i64,
        }
        diesel::sql_query("SELECT updated_at FROM wuphf_contacts WHERE jid = ?")
            .bind::<Text, _>(jid)
            .get_result::<Row>(conn)
            .unwrap()
            .updated_at
    }

    fn forms(forms: &[&str]) -> Vec<String> {
        forms.iter().map(|form| (*form).to_owned()).collect()
    }

    #[test]
    fn push_names_are_written_only_when_they_change() {
        let mut conn = memory();
        let jid = "1555@s.whatsapp.net";
        assert!(upsert_push_name(&mut conn, jid, "Alice", 10).unwrap());
        assert!(!upsert_push_name(&mut conn, jid, "Alice", 20).unwrap());
        assert_eq!(updated_at(&mut conn, jid), 10);
        assert!(upsert_push_name(&mut conn, jid, "Ally", 30).unwrap());
        assert_eq!(updated_at(&mut conn, jid), 30);
        assert_eq!(
            lookup(&mut conn, &forms(&[jid])).unwrap(),
            ContactNames {
                push_name: Some("Ally".into()),
                ..Default::default()
            }
        );
    }

    #[test]
    fn contact_names_keep_the_push_name_and_skip_unchanged_values() {
        let mut conn = memory();
        let jid = "1555@s.whatsapp.net";
        assert!(!upsert_contact_name(&mut conn, jid, None, None, 5).unwrap());
        upsert_push_name(&mut conn, jid, "Alice", 10).unwrap();
        assert!(
            upsert_contact_name(&mut conn, jid, Some("Alice Smith"), Some("Alice"), 20).unwrap()
        );
        assert!(
            !upsert_contact_name(&mut conn, jid, Some("Alice Smith"), Some("Alice"), 30).unwrap()
        );
        assert_eq!(updated_at(&mut conn, jid), 20);
        assert_eq!(
            lookup(&mut conn, &forms(&[jid])).unwrap(),
            ContactNames {
                full_name: Some("Alice Smith".into()),
                first_name: Some("Alice".into()),
                push_name: Some("Alice".into()),
            }
        );
        // A later sync without names clears them; the push name stays.
        assert!(upsert_contact_name(&mut conn, jid, None, None, 40).unwrap());
        assert_eq!(
            lookup(&mut conn, &forms(&[jid])).unwrap(),
            ContactNames {
                push_name: Some("Alice".into()),
                ..Default::default()
            }
        );
    }

    #[test]
    fn a_lookup_merges_the_lid_and_phone_forms() {
        let mut conn = memory();
        // The push name arrived under the LID, the saved name under the phone.
        upsert_push_name(&mut conn, "8888@lid", "Ali", 1).unwrap();
        upsert_contact_name(
            &mut conn,
            "1555@s.whatsapp.net",
            Some("Alice Smith"),
            None,
            1,
        )
        .unwrap();
        let names = lookup(&mut conn, &forms(&["8888@lid", "1555@s.whatsapp.net"])).unwrap();
        assert_eq!(names.full_name.as_deref(), Some("Alice Smith"));
        assert_eq!(names.push_name.as_deref(), Some("Ali"));
        // The first form wins a field both know.
        upsert_push_name(&mut conn, "1555@s.whatsapp.net", "Alice", 2).unwrap();
        let names = lookup(&mut conn, &forms(&["1555@s.whatsapp.net", "8888@lid"])).unwrap();
        assert_eq!(names.push_name.as_deref(), Some("Alice"));
        // Unknown forms know nothing.
        assert_eq!(
            lookup(&mut conn, &forms(&["2@s.whatsapp.net"])).unwrap(),
            ContactNames::default()
        );
        assert!(!ContactNames::default().is_named());
    }

    #[test]
    fn a_push_name_list_skips_empty_names() {
        let mut conn = memory();
        store_push_names(
            &mut conn,
            &[
                ("1@s.whatsapp.net".into(), " Bob ".into()),
                ("2@s.whatsapp.net".into(), "  ".into()),
            ],
            1,
        )
        .unwrap();
        assert_eq!(
            lookup(&mut conn, &forms(&["1@s.whatsapp.net"]))
                .unwrap()
                .push_name
                .as_deref(),
            Some("Bob")
        );
        assert_eq!(
            lookup(&mut conn, &forms(&["2@s.whatsapp.net"])).unwrap(),
            ContactNames::default()
        );
    }
}
