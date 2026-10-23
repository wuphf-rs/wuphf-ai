//! Data migrations that rewrite stored JSON payloads. Each test builds a
//! database only up to the migration under test, seeds rows in the JSON shape
//! an older release wrote, applies the remaining migrations, and checks every
//! row decodes into today's model.

use crate::sqlite::MIGRATOR;
use serde::Serialize;
use serde::de::DeserializeOwned;
use sqlx::migrate::Migrator;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};
use std::fmt::Debug;
use std::str::FromStr;
use uuid::Uuid;
use wuphf_model::{
    Action, Attachment, AttachmentKind, ChatAddress, ConnectorId, Dispatch, DispatchCommand,
    DispatchId, DispatchState, EndpointId, MediaId, Message, MessageCopy, MessageId,
    ObservedDirection, ObservedMessage, Platform, ProvisionThread, ReplicaSetId, ReplyContext,
    TargetAddress,
};

/// A fresh database migrated up to, but not including, `version`.
async fn database_before(version: i64) -> SqlitePool {
    let path = std::env::temp_dir().join(format!("wuphf-migrate-{}.sqlite", DispatchId::new()));
    let options = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))
        .unwrap()
        .create_if_missing(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap();
    let earlier = Migrator {
        migrations: MIGRATOR
            .iter()
            .filter(|migration| migration.version < version)
            .cloned()
            .collect::<Vec<_>>()
            .into(),
        ..Migrator::DEFAULT
    };
    earlier.run(&pool).await.unwrap();
    pool
}

/// Assert the `payload` of the row in `table` whose `key` column is `id` is
/// exactly the JSON today's model writes for `expected`, and decodes back to it.
/// Comparing the text, not only the decoded value, also catches a migration
/// that leaves a stale key behind for serde to ignore.
async fn assert_stored<T>(pool: &SqlitePool, table: &str, key: &str, id: &str, expected: &T)
where
    T: Serialize + DeserializeOwned + PartialEq + Debug,
{
    let row = sqlx::query(&format!("SELECT payload FROM {table} WHERE {key} = ?"))
        .bind(id)
        .fetch_one(pool)
        .await
        .unwrap();
    let stored: String = row.get("payload");
    assert_eq!(
        stored,
        serde_json::to_string(expected).unwrap(),
        "{table} {id}"
    );
    assert_eq!(&serde_json::from_str::<T>(&stored).unwrap(), expected);
}

/// Migration 0011 renames the JSON keys of every stored message, copy,
/// dispatch, and observed message to the current model vocabulary.
#[tokio::test]
async fn migration_0011_rewrites_stored_json_to_current_names() {
    let pool = database_before(11).await;
    sqlx::query("INSERT INTO messages (id, payload) VALUES (?, ?)")
        .bind(message_id().to_string())
        .bind(LEGACY_MESSAGE)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO message_copies (message_id, endpoint_id, platform_message_id, sent, payload)
         VALUES (?, ?, 'tg-1', 1, ?)",
    )
    .bind(message_id().to_string())
    .bind(endpoint_id().to_string())
    .bind(LEGACY_COPY)
    .execute(&pool)
    .await
    .unwrap();
    for (dispatch, json) in sample_dispatches().iter().zip(LEGACY_DISPATCHES) {
        sqlx::query(
            "INSERT INTO dispatches (id, target_endpoint, message_id, waiting_for, state, payload)
             VALUES (?, ?, NULL, NULL, 'pending', ?)",
        )
        .bind(dispatch.id.to_string())
        .bind(endpoint_id().to_string())
        .bind(json)
        .execute(&pool)
        .await
        .unwrap();
    }
    sqlx::query(
        "INSERT INTO observed_messages (connector_id, chat_id, platform_message_id, timestamp_ms, payload)
         VALUES ('whatsapp', 'wa-chat', 'wa-1', 1000, ?)",
    )
    .bind(LEGACY_OBSERVED)
    .execute(&pool)
    .await
    .unwrap();

    MIGRATOR.run(&pool).await.unwrap();

    let message_key = message_id().to_string();
    assert_stored(&pool, "messages", "id", &message_key, &sample_message()).await;
    assert_stored(
        &pool,
        "message_copies",
        "message_id",
        &message_key,
        &sample_copy(),
    )
    .await;
    for expected in sample_dispatches() {
        assert_stored(
            &pool,
            "dispatches",
            "id",
            &expected.id.to_string(),
            &expected,
        )
        .await;
    }
    assert_stored(
        &pool,
        "observed_messages",
        "platform_message_id",
        "wa-1",
        &sample_observed(),
    )
    .await;
}

/// Prints the JSON today's model writes for each sample. Run it to capture a new
/// `LEGACY_` fixture before a migration renames stored keys.
#[test]
#[ignore]
fn print_current_json() {
    println!(
        "MESSAGE {}",
        serde_json::to_string(&sample_message()).unwrap()
    );
    println!("COPY {}", serde_json::to_string(&sample_copy()).unwrap());
    for dispatch in sample_dispatches() {
        println!("DISPATCH {}", serde_json::to_string(&dispatch).unwrap());
    }
    println!(
        "OBSERVED {}",
        serde_json::to_string(&sample_observed()).unwrap()
    );
}

fn message_id() -> MessageId {
    MessageId(Uuid::from_u128(1))
}

fn endpoint_id() -> EndpointId {
    EndpointId(Uuid::from_u128(2))
}

fn replica_set_id() -> ReplicaSetId {
    ReplicaSetId(Uuid::from_u128(3))
}

fn attachment() -> Attachment {
    Attachment {
        media_id: MediaId(Uuid::from_u128(4)),
        kind: AttachmentKind::Image,
        file_name: Some("photo.jpg".into()),
        mime_type: Some("image/jpeg".into()),
        size_bytes: Some(1234),
        animated: false,
    }
}

fn sample_message() -> Message {
    Message {
        id: message_id(),
        replica_set_id: replica_set_id(),
        author_id: "15550001111@s.whatsapp.net".into(),
        participant: Some("15550001111@s.whatsapp.net".into()),
        author_name: Some("Alice".into()),
        text: Some("hello".into()),
        attachments: vec![attachment()],
        reply_to: Some(MessageId(Uuid::from_u128(5))),
        timestamp_ms: 1000,
        deleted: false,
    }
}

fn sample_copy() -> MessageCopy {
    MessageCopy {
        message_id: message_id(),
        endpoint_id: endpoint_id(),
        native_id: "tg-1".into(),
    }
}

fn command(action: Action, create_thread: Option<ProvisionThread>) -> DispatchCommand {
    DispatchCommand {
        target_endpoint: endpoint_id(),
        target_connector: ConnectorId("telegram".into()),
        target: TargetAddress {
            address: ChatAddress::new("-100", Some("7".into())),
            create_thread,
        },
        payload: action,
    }
}

/// One dispatch per [`Action`] variant. The create also carries a reply, a
/// thread to provision, and an acknowledged state, so every renamed key on a
/// dispatch appears at least once.
fn sample_dispatches() -> Vec<Dispatch> {
    let dispatch = |n: u128, action: Action, state: DispatchState| Dispatch {
        id: DispatchId(Uuid::from_u128(100 + n)),
        command: command(action, None),
        state,
    };
    let create = Dispatch {
        id: DispatchId(Uuid::from_u128(100)),
        command: command(
            Action::Send {
                message: sample_message(),
                reply: Some(ReplyContext {
                    message_id: MessageId(Uuid::from_u128(5)),
                    native_id: "tg-parent".into(),
                    author_id: "15550002222@s.whatsapp.net".into(),
                    participant: None,
                    text: Some("parent".into()),
                    attachments: Vec::new(),
                    authored_by_bridge: true,
                }),
            },
            Some(ProvisionThread {
                container: "-100".into(),
                title: "Alice".into(),
            }),
        ),
        state: DispatchState::Acknowledged {
            native_id: "tg-1".into(),
        },
    };
    vec![
        create,
        dispatch(
            1,
            Action::Edit {
                message: sample_message(),
                native_id: "tg-1".into(),
            },
            DispatchState::Pending,
        ),
        dispatch(
            2,
            Action::Delete {
                message: sample_message(),
                native_id: "tg-1".into(),
            },
            DispatchState::Pending,
        ),
        dispatch(
            3,
            Action::RenameThread {
                replica_set_id: replica_set_id(),
                title: "Alice (renamed)".into(),
            },
            DispatchState::Pending,
        ),
        dispatch(
            4,
            Action::React {
                message_id: message_id(),
                replica_set_id: replica_set_id(),
                native_id: "tg-1".into(),
                reactor: "15550002222@s.whatsapp.net".into(),
                reactor_name: Some("Bob".into()),
                emoji: Some("👍".into()),
            },
            DispatchState::Pending,
        ),
        dispatch(
            5,
            Action::DeliveryAck {
                message_id: message_id(),
                replica_set_id: replica_set_id(),
                native_id: "wa-1".into(),
            },
            DispatchState::Pending,
        ),
        dispatch(
            6,
            Action::SeenReceipt {
                message_id: message_id(),
                replica_set_id: replica_set_id(),
                native_id: "wa-1".into(),
            },
            DispatchState::Pending,
        ),
    ]
}

fn sample_observed() -> ObservedMessage {
    ObservedMessage {
        connector_id: ConnectorId("whatsapp".into()),
        platform: Platform::WhatsApp,
        chat_id: "wa-chat".into(),
        thread_id: Some("thread".into()),
        native_id: "wa-1".into(),
        author_id: "15550001111@s.whatsapp.net".into(),
        author_name: Some("Alice".into()),
        participant: Some("15550001111@s.whatsapp.net".into()),
        text: Some("hello".into()),
        attachments: vec![attachment()],
        reply_to_native_id: Some("wa-0".into()),
        direction: ObservedDirection::Incoming,
        timestamp_ms: 1000,
        deleted: true,
        edited: true,
    }
}

// JSON as written before migration 0011, captured with `print_current_json`.
const LEGACY_MESSAGE: &str = r#"{"id":"00000000-0000-0000-0000-000000000001","replica_set_id":"00000000-0000-0000-0000-000000000003","sender":"15550001111@s.whatsapp.net","participant":"15550001111@s.whatsapp.net","sender_name":"Alice","text":"hello","attachments":[{"media_id":"00000000-0000-0000-0000-000000000004","kind":"image","file_name":"photo.jpg","mime_type":"image/jpeg","size_bytes":1234,"animated":false}],"reply_to":"00000000-0000-0000-0000-000000000005","timestamp_ms":1000,"deleted":false}"#;
const LEGACY_COPY: &str = r#"{"message_id":"00000000-0000-0000-0000-000000000001","endpoint_id":"00000000-0000-0000-0000-000000000002","platform_message_id":"tg-1"}"#;
const LEGACY_DISPATCHES: [&str; 7] = [
    r#"{"id":"00000000-0000-0000-0000-000000000064","command":{"target_endpoint":"00000000-0000-0000-0000-000000000002","target_connector":"telegram","target":{"address":{"chat_id":"-100","thread_id":"7"},"provision_thread":{"container":"-100","title":"Alice"}},"payload":{"MessageCreate":{"message":{"id":"00000000-0000-0000-0000-000000000001","replica_set_id":"00000000-0000-0000-0000-000000000003","sender":"15550001111@s.whatsapp.net","participant":"15550001111@s.whatsapp.net","sender_name":"Alice","text":"hello","attachments":[{"media_id":"00000000-0000-0000-0000-000000000004","kind":"image","file_name":"photo.jpg","mime_type":"image/jpeg","size_bytes":1234,"animated":false}],"reply_to":"00000000-0000-0000-0000-000000000005","timestamp_ms":1000,"deleted":false},"reply_context":{"message_id":"00000000-0000-0000-0000-000000000005","platform_message_id":"tg-parent","sender":"15550002222@s.whatsapp.net","participant":null,"text":"parent","attachments":[],"authored_by_bridge":true}}}},"state":{"Acknowledged":{"platform_message_id":"tg-1"}}}"#,
    r#"{"id":"00000000-0000-0000-0000-000000000065","command":{"target_endpoint":"00000000-0000-0000-0000-000000000002","target_connector":"telegram","target":{"address":{"chat_id":"-100","thread_id":"7"},"provision_thread":null},"payload":{"MessageEdit":{"message":{"id":"00000000-0000-0000-0000-000000000001","replica_set_id":"00000000-0000-0000-0000-000000000003","sender":"15550001111@s.whatsapp.net","participant":"15550001111@s.whatsapp.net","sender_name":"Alice","text":"hello","attachments":[{"media_id":"00000000-0000-0000-0000-000000000004","kind":"image","file_name":"photo.jpg","mime_type":"image/jpeg","size_bytes":1234,"animated":false}],"reply_to":"00000000-0000-0000-0000-000000000005","timestamp_ms":1000,"deleted":false},"target_platform_message_id":"tg-1"}}},"state":"Pending"}"#,
    r#"{"id":"00000000-0000-0000-0000-000000000066","command":{"target_endpoint":"00000000-0000-0000-0000-000000000002","target_connector":"telegram","target":{"address":{"chat_id":"-100","thread_id":"7"},"provision_thread":null},"payload":{"MessageDelete":{"message":{"id":"00000000-0000-0000-0000-000000000001","replica_set_id":"00000000-0000-0000-0000-000000000003","sender":"15550001111@s.whatsapp.net","participant":"15550001111@s.whatsapp.net","sender_name":"Alice","text":"hello","attachments":[{"media_id":"00000000-0000-0000-0000-000000000004","kind":"image","file_name":"photo.jpg","mime_type":"image/jpeg","size_bytes":1234,"animated":false}],"reply_to":"00000000-0000-0000-0000-000000000005","timestamp_ms":1000,"deleted":false},"target_platform_message_id":"tg-1"}}},"state":"Pending"}"#,
    r#"{"id":"00000000-0000-0000-0000-000000000067","command":{"target_endpoint":"00000000-0000-0000-0000-000000000002","target_connector":"telegram","target":{"address":{"chat_id":"-100","thread_id":"7"},"provision_thread":null},"payload":{"RetitleThread":{"replica_set_id":"00000000-0000-0000-0000-000000000003","title":"Alice (renamed)"}}},"state":"Pending"}"#,
    r#"{"id":"00000000-0000-0000-0000-000000000068","command":{"target_endpoint":"00000000-0000-0000-0000-000000000002","target_connector":"telegram","target":{"address":{"chat_id":"-100","thread_id":"7"},"provision_thread":null},"payload":{"Reaction":{"message_id":"00000000-0000-0000-0000-000000000001","replica_set_id":"00000000-0000-0000-0000-000000000003","target_platform_message_id":"tg-1","reactor":"15550002222@s.whatsapp.net","reactor_name":"Bob","emoji":"👍"}}},"state":"Pending"}"#,
    r#"{"id":"00000000-0000-0000-0000-000000000069","command":{"target_endpoint":"00000000-0000-0000-0000-000000000002","target_connector":"telegram","target":{"address":{"chat_id":"-100","thread_id":"7"},"provision_thread":null},"payload":{"DeliveryAck":{"message_id":"00000000-0000-0000-0000-000000000001","replica_set_id":"00000000-0000-0000-0000-000000000003","target_platform_message_id":"wa-1"}}},"state":"Pending"}"#,
    r#"{"id":"00000000-0000-0000-0000-00000000006a","command":{"target_endpoint":"00000000-0000-0000-0000-000000000002","target_connector":"telegram","target":{"address":{"chat_id":"-100","thread_id":"7"},"provision_thread":null},"payload":{"SeenReceipt":{"message_id":"00000000-0000-0000-0000-000000000001","replica_set_id":"00000000-0000-0000-0000-000000000003","target_platform_message_id":"wa-1"}}},"state":"Pending"}"#,
];
const LEGACY_OBSERVED: &str = r#"{"connector_id":"whatsapp","platform":"whats_app","chat_id":"wa-chat","thread_id":"thread","platform_message_id":"wa-1","sender":"15550001111@s.whatsapp.net","sender_name":"Alice","participant":"15550001111@s.whatsapp.net","text":"hello","attachments":[{"media_id":"00000000-0000-0000-0000-000000000004","kind":"image","file_name":"photo.jpg","mime_type":"image/jpeg","size_bytes":1234,"animated":false}],"reply_to_platform_message_id":"wa-0","direction":"incoming","timestamp_ms":1000,"deleted":true,"edited":true}"#;
