//! Data migrations that rewrite stored JSON payloads. Each test builds a
//! database only up to the migration under test, seeds rows in the JSON shape
//! an older release wrote, applies the remaining migrations, and checks every
//! row decodes into today's model.

use crate::Storage;
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
    Action, Attachment, AttachmentKind, ChatAddress, ChatRef, ConnectorId, Conversation,
    ConversationId, Dispatch, DispatchId, DispatchState, Endpoint, EndpointId, MediaId, Message,
    MessageCopy, MessageId, ObservedChat, ObservedDirection, ObservedMessage, Platform,
    ReceiptKind, ReplyContext, RuleId, Target,
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
/// dispatch, and observed message to the current model vocabulary. The rows
/// also pass through every later migration, so they must end in today's shape.
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
    insert_dispatches(&pool, &LEGACY_DISPATCHES).await;
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
    assert_dispatches(&pool).await;
    assert_stored(
        &pool,
        "observed_messages",
        "platform_message_id",
        "wa-1",
        &sample_observed(),
    )
    .await;
}

/// Migration 0013 reshapes stored dispatches: the command's target fields move
/// into `to`, the replica set id is hoisted onto the dispatch, a thread to
/// create shrinks to its title, and delivery acks and seen receipts become one
/// `Receipt` action.
#[tokio::test]
async fn migration_0013_reshapes_stored_dispatches() {
    let pool = database_before(13).await;
    insert_dispatches(&pool, &DISPATCHES_BEFORE_0013).await;

    MIGRATOR.run(&pool).await.unwrap();

    assert_dispatches(&pool).await;
}

/// Migration 0014 merges each replica set into its conversation (which takes
/// the replica set's id), turns a replica set with no conversation (an owner
/// message to a chat outside every rule) into a rule-less conversation, indexes
/// endpoints by conversation, and drops the replica set and thread mapping
/// tables.
#[tokio::test]
async fn migration_0014_merges_replica_sets_into_conversations() {
    let pool = database_before(14).await;
    let [wa, tg, standalone] = [21, 22, 23].map(|n| EndpointId(Uuid::from_u128(n)));
    let [mirrored, rule_less] = [31, 32].map(|n| ConversationId(Uuid::from_u128(n)));
    let old_conversation = Uuid::from_u128(41);
    let seed = format!(
        r#"
        INSERT INTO endpoints (id, payload) VALUES
            ('{wa}', '{{"id":"{wa}","connector_id":"whatsapp","platform":"whats_app","chat_id":"wa-1","thread_id":null}}'),
            ('{tg}', '{{"id":"{tg}","connector_id":"telegram","platform":"telegram","chat_id":"tg-1","thread_id":"57"}}'),
            ('{standalone}', '{{"id":"{standalone}","connector_id":"whatsapp","platform":"whats_app","chat_id":"wa-2","thread_id":null}}');
        INSERT INTO replica_sets (id, payload) VALUES
            ('{mirrored}', '{{"id":"{mirrored}","endpoints":["{wa}","{tg}"]}}'),
            ('{rule_less}', '{{"id":"{rule_less}","endpoints":["{standalone}"]}}');
        INSERT INTO conversations
            (id, rule_id, replica_set_id, source_connector, source_chat_id, source_thread_key, payload)
        VALUES ('{old_conversation}', 'mirror', '{mirrored}', 'whatsapp', 'wa-1', '',
            '{{"id":"{old_conversation}","rule_id":"mirror","replica_set_id":"{mirrored}","source_connector":"whatsapp","source_address":{{"chat_id":"wa-1","thread_id":null}},"title":"Priya"}}');
        INSERT INTO thread_mappings (conversation_id, connector_id, container, thread_id)
            VALUES ('{old_conversation}', 'telegram', 'tg-1', '57');
        INSERT INTO provisioning_stash (replica_set_id, message_id, origin_endpoint, provision_title)
            VALUES ('{mirrored}', '{message}', '{wa}', 'Priya');
        "#,
        message = message_id(),
    );
    sqlx::raw_sql(&seed).execute(&pool).await.unwrap();

    MIGRATOR.run(&pool).await.unwrap();

    let source = |chat_id: &str| ChatRef {
        connector_id: ConnectorId("whatsapp".into()),
        address: ChatAddress::new(chat_id, None),
    };
    let expected = [
        Conversation {
            id: mirrored,
            rule_id: Some(RuleId("mirror".into())),
            source: source("wa-1"),
            endpoints: vec![wa, tg],
            title: Some("Priya".into()),
        },
        Conversation {
            id: rule_less,
            rule_id: None,
            source: source("wa-2"),
            endpoints: vec![standalone],
            title: None,
        },
    ];
    for conversation in &expected {
        assert_stored(
            &pool,
            "conversations",
            "id",
            &conversation.id.to_string(),
            conversation,
        )
        .await;
    }
    let rule_ids: Vec<Option<String>> =
        sqlx::query_scalar("SELECT rule_id FROM conversations ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(rule_ids, vec![Some("mirror".to_owned()), None]);

    let index: Vec<(String, String)> = sqlx::query_as(
        "SELECT endpoint_id, conversation_id FROM conversation_endpoints ORDER BY endpoint_id",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    let expected_index = [(wa, mirrored), (tg, mirrored), (standalone, rule_less)]
        .map(|(endpoint, conversation)| (endpoint.to_string(), conversation.to_string()));
    assert_eq!(index, expected_index);

    let dropped: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM sqlite_master WHERE name IN ('replica_sets', 'thread_mappings')",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(dropped, 0);
    let stashed: String = sqlx::query_scalar("SELECT conversation_id FROM provisioning_stash")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(stashed, mirrored.to_string());
}

/// Seed one `dispatches` row per sample, in the JSON shape given.
async fn insert_dispatches(pool: &SqlitePool, payloads: &[&str]) {
    for (dispatch, payload) in sample_dispatches().iter().zip(payloads) {
        sqlx::query(
            "INSERT INTO dispatches (id, target_endpoint, message_id, waiting_for, state, payload)
             VALUES (?, ?, NULL, NULL, 'pending', ?)",
        )
        .bind(dispatch.id.to_string())
        .bind(endpoint_id().to_string())
        .bind(payload)
        .execute(pool)
        .await
        .unwrap();
    }
}

/// Every sample dispatch is stored exactly as today's model writes it.
async fn assert_dispatches(pool: &SqlitePool) {
    for expected in sample_dispatches() {
        assert_stored(
            pool,
            "dispatches",
            "id",
            &expected.id.to_string(),
            &expected,
        )
        .await;
    }
}

/// Migrates a copy of a real database and checks every stored row still
/// decodes into today's model. Opt-in: point `WUPHF_MIGRATION_DB` at a COPY of
/// the database (the test migrates it in place) and run with `--ignored`.
#[tokio::test]
#[ignore]
async fn real_database_migrates_and_decodes() {
    let path = std::env::var("WUPHF_MIGRATION_DB")
        .expect("set WUPHF_MIGRATION_DB to a copy of the database to migrate");
    let storage = crate::SqliteStorage::connect(&format!("sqlite://{path}"))
        .await
        .unwrap();
    let pool = SqlitePool::connect(&format!("sqlite://{path}"))
        .await
        .unwrap();

    let endpoints = decode_all::<Endpoint>(&pool, "endpoints").await;
    let conversations = decode_all::<Conversation>(&pool, "conversations").await;
    decode_all::<Message>(&pool, "messages").await;
    decode_all::<MessageCopy>(&pool, "message_copies").await;
    decode_all::<Dispatch>(&pool, "dispatches").await;
    decode_all::<ObservedMessage>(&pool, "observed_messages").await;
    decode_all::<ObservedChat>(&pool, "observed_chats").await;

    // Every endpoint a conversation lists resolves back to that conversation
    // through the index, and every listed endpoint exists.
    for conversation in &conversations {
        for endpoint_id in &conversation.endpoints {
            assert!(endpoints.iter().any(|endpoint| endpoint.id == *endpoint_id));
            let found = storage
                .conversation_for_endpoint(*endpoint_id)
                .await
                .unwrap()
                .expect("a listed endpoint is indexed");
            assert_eq!(found.id, conversation.id);
        }
    }
}

/// Decode every `payload` in `table`, failing on the first row that does not
/// fit the model.
async fn decode_all<T: DeserializeOwned>(pool: &SqlitePool, table: &str) -> Vec<T> {
    let rows = sqlx::query(&format!("SELECT payload FROM {table}"))
        .fetch_all(pool)
        .await
        .unwrap();
    rows.iter()
        .map(|row| {
            let payload: String = row.get("payload");
            serde_json::from_str(&payload)
                .unwrap_or_else(|error| panic!("{table} row does not decode: {error}\n{payload}"))
        })
        .collect()
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

fn conversation_id() -> ConversationId {
    ConversationId(Uuid::from_u128(3))
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
        conversation_id: conversation_id(),
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

fn target(create_thread: Option<String>) -> Target {
    Target {
        endpoint_id: endpoint_id(),
        connector_id: ConnectorId("telegram".into()),
        address: ChatAddress::new("-100", Some("7".into())),
        create_thread,
    }
}

/// One dispatch per kind of stored action. The send also carries a reply, a
/// thread to create, and an acknowledged state, so every key a migration
/// rewrites on a dispatch appears at least once.
fn sample_dispatches() -> Vec<Dispatch> {
    let dispatch = |n: u128, action: Action, state: DispatchState| Dispatch {
        id: DispatchId(Uuid::from_u128(100 + n)),
        state,
        to: target(None),
        conversation_id: conversation_id(),
        action,
    };
    let send = Dispatch {
        id: DispatchId(Uuid::from_u128(100)),
        state: DispatchState::Acknowledged {
            native_id: "tg-1".into(),
        },
        to: target(Some("Alice".into())),
        conversation_id: conversation_id(),
        action: Action::Send {
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
    };
    vec![
        send,
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
                message_id: message_id(),
                native_id: "tg-1".into(),
            },
            DispatchState::Pending,
        ),
        dispatch(
            3,
            Action::RenameThread {
                title: "Alice (renamed)".into(),
            },
            DispatchState::Pending,
        ),
        dispatch(
            4,
            Action::React {
                message_id: message_id(),
                native_id: "tg-1".into(),
                reactor: "15550002222@s.whatsapp.net".into(),
                reactor_name: Some("Bob".into()),
                emoji: Some("👍".into()),
            },
            DispatchState::Pending,
        ),
        dispatch(
            5,
            Action::Receipt {
                message_id: message_id(),
                native_id: "wa-1".into(),
                kind: ReceiptKind::Delivered,
            },
            DispatchState::Pending,
        ),
        dispatch(
            6,
            Action::Receipt {
                message_id: message_id(),
                native_id: "wa-1".into(),
                kind: ReceiptKind::Read,
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

// Dispatch JSON as written before migration 0013, captured with
// `print_current_json`.
const DISPATCHES_BEFORE_0013: [&str; 7] = [
    r#"{"id":"00000000-0000-0000-0000-000000000064","command":{"target_endpoint":"00000000-0000-0000-0000-000000000002","target_connector":"telegram","target":{"address":{"chat_id":"-100","thread_id":"7"},"create_thread":{"container":"-100","title":"Alice"}},"payload":{"Send":{"message":{"id":"00000000-0000-0000-0000-000000000001","replica_set_id":"00000000-0000-0000-0000-000000000003","author_id":"15550001111@s.whatsapp.net","participant":"15550001111@s.whatsapp.net","author_name":"Alice","text":"hello","attachments":[{"media_id":"00000000-0000-0000-0000-000000000004","kind":"image","file_name":"photo.jpg","mime_type":"image/jpeg","size_bytes":1234,"animated":false}],"reply_to":"00000000-0000-0000-0000-000000000005","timestamp_ms":1000,"deleted":false},"reply":{"message_id":"00000000-0000-0000-0000-000000000005","native_id":"tg-parent","author_id":"15550002222@s.whatsapp.net","participant":null,"text":"parent","attachments":[],"authored_by_bridge":true}}}},"state":{"Acknowledged":{"native_id":"tg-1"}}}"#,
    r#"{"id":"00000000-0000-0000-0000-000000000065","command":{"target_endpoint":"00000000-0000-0000-0000-000000000002","target_connector":"telegram","target":{"address":{"chat_id":"-100","thread_id":"7"},"create_thread":null},"payload":{"Edit":{"message":{"id":"00000000-0000-0000-0000-000000000001","replica_set_id":"00000000-0000-0000-0000-000000000003","author_id":"15550001111@s.whatsapp.net","participant":"15550001111@s.whatsapp.net","author_name":"Alice","text":"hello","attachments":[{"media_id":"00000000-0000-0000-0000-000000000004","kind":"image","file_name":"photo.jpg","mime_type":"image/jpeg","size_bytes":1234,"animated":false}],"reply_to":"00000000-0000-0000-0000-000000000005","timestamp_ms":1000,"deleted":false},"native_id":"tg-1"}}},"state":"Pending"}"#,
    r#"{"id":"00000000-0000-0000-0000-000000000066","command":{"target_endpoint":"00000000-0000-0000-0000-000000000002","target_connector":"telegram","target":{"address":{"chat_id":"-100","thread_id":"7"},"create_thread":null},"payload":{"Delete":{"message":{"id":"00000000-0000-0000-0000-000000000001","replica_set_id":"00000000-0000-0000-0000-000000000003","author_id":"15550001111@s.whatsapp.net","participant":"15550001111@s.whatsapp.net","author_name":"Alice","text":"hello","attachments":[{"media_id":"00000000-0000-0000-0000-000000000004","kind":"image","file_name":"photo.jpg","mime_type":"image/jpeg","size_bytes":1234,"animated":false}],"reply_to":"00000000-0000-0000-0000-000000000005","timestamp_ms":1000,"deleted":false},"native_id":"tg-1"}}},"state":"Pending"}"#,
    r#"{"id":"00000000-0000-0000-0000-000000000067","command":{"target_endpoint":"00000000-0000-0000-0000-000000000002","target_connector":"telegram","target":{"address":{"chat_id":"-100","thread_id":"7"},"create_thread":null},"payload":{"RenameThread":{"replica_set_id":"00000000-0000-0000-0000-000000000003","title":"Alice (renamed)"}}},"state":"Pending"}"#,
    r#"{"id":"00000000-0000-0000-0000-000000000068","command":{"target_endpoint":"00000000-0000-0000-0000-000000000002","target_connector":"telegram","target":{"address":{"chat_id":"-100","thread_id":"7"},"create_thread":null},"payload":{"React":{"message_id":"00000000-0000-0000-0000-000000000001","replica_set_id":"00000000-0000-0000-0000-000000000003","native_id":"tg-1","reactor":"15550002222@s.whatsapp.net","reactor_name":"Bob","emoji":"👍"}}},"state":"Pending"}"#,
    r#"{"id":"00000000-0000-0000-0000-000000000069","command":{"target_endpoint":"00000000-0000-0000-0000-000000000002","target_connector":"telegram","target":{"address":{"chat_id":"-100","thread_id":"7"},"create_thread":null},"payload":{"DeliveryAck":{"message_id":"00000000-0000-0000-0000-000000000001","replica_set_id":"00000000-0000-0000-0000-000000000003","native_id":"wa-1"}}},"state":"Pending"}"#,
    r#"{"id":"00000000-0000-0000-0000-00000000006a","command":{"target_endpoint":"00000000-0000-0000-0000-000000000002","target_connector":"telegram","target":{"address":{"chat_id":"-100","thread_id":"7"},"create_thread":null},"payload":{"SeenReceipt":{"message_id":"00000000-0000-0000-0000-000000000001","replica_set_id":"00000000-0000-0000-0000-000000000003","native_id":"wa-1"}}},"state":"Pending"}"#,
];
