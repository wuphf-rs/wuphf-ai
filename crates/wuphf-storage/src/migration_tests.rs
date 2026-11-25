//! Data migrations that rewrite stored JSON payloads. Each test builds a
//! database only up to the migration under test, seeds rows in the JSON shape
//! an older release wrote, applies the remaining migrations, and checks every
//! row decodes into today's model.

use crate::TopologyStore;
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
    Action, Attachment, AttachmentKind, ChatAddress, ChatProfile, ChatRef, ConnectorId,
    Conversation, ConversationId, DisappearingTimer, Dispatch, DispatchId, DispatchState, Endpoint,
    EndpointId, MediaId, Message, MessageAuthor, MessageCopy, MessageId, ObservedChat,
    ObservedDirection, ObservedMessage, Platform, ReceiptKind, ReplyContext, RuleId, Target,
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
    migrator_before(version).run(&pool).await.unwrap();
    pool
}

/// The migrations before `version`.
fn migrator_before(version: i64) -> Migrator {
    Migrator {
        migrations: MIGRATOR
            .iter()
            .filter(|migration| migration.version < version)
            .cloned()
            .collect::<Vec<_>>()
            .into(),
        ..Migrator::DEFAULT
    }
}

/// Assert the `payload` of the row in `table` whose `key` column is `id` is
/// exactly the JSON today's model writes for `expected`, and decodes back to it.
/// Comparing the text, not only the decoded value, also catches a migration
/// that leaves a stale key behind for serde to ignore.
async fn assert_stored<T>(pool: &SqlitePool, table: &str, key: &str, id: &str, expected: &T)
where
    T: Serialize + DeserializeOwned + PartialEq + Debug,
{
    let row = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT payload FROM {table} WHERE {key} = ?"
    )))
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
    sqlx::raw_sql(sqlx::AssertSqlSafe(seed))
        .execute(&pool)
        .await
        .unwrap();

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
            introduced: false,
        },
        Conversation {
            id: rule_less,
            rule_id: None,
            source: source("wa-2"),
            endpoints: vec![standalone],
            title: None,
            introduced: false,
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

/// Migration 0016 rewrites the `rules` and `notify` settings overrides from
/// the shapes plan 037 stored (a list of `{id, flow}`, a list of chat objects)
/// into plan 038's config shapes (a map keyed by id with the list position as
/// `priority`, chats as `connector:chat#thread` strings). Other settings are
/// left alone.
#[tokio::test]
async fn migration_0016_reshapes_stored_rules_and_notify() {
    let pool = database_before(16).await;
    let rules = serde_json::json!([
        {
            "id": "direct",
            "flow": {
                "kind": "one-to-one",
                "source_connector": "whatsapp-main",
                "source_chat": { "chat_id": "123@s.whatsapp.net", "thread_id": null },
                "destination_connector": "telegram-main",
                "destination_chat": { "chat_id": "-100123", "thread_id": "12" }
            }
        },
        {
            "id": "mirror",
            "flow": {
                "kind": "many-to-one",
                "source_connector": "whatsapp-main",
                "source_chats_filter": {
                    "kind": "not",
                    "chats": [{ "chat_id": "a#b@g.us", "thread_id": null }]
                },
                "destination_connector": "telegram-main",
                "destination_chat": { "chat_id": "-100999", "thread_id": null }
            }
        }
    ]);
    let notify = serde_json::json!([
        { "connector_id": "telegram-main", "chat_id": "telegram:user:1", "thread_id": "5" },
        { "connector_id": "whatsapp-main", "chat_id": "x@s.whatsapp.net" }
    ]);
    let unrelated = r#"{"kept":  "byte for byte"}"#;
    for (key, value) in [
        ("rules", rules.to_string()),
        ("notify", notify.to_string()),
        ("timezone", unrelated.to_owned()),
    ] {
        sqlx::query(
            "INSERT INTO settings (key, value, updated_by, updated_at_ms) VALUES (?, ?, NULL, 0)",
        )
        .bind(key)
        .bind(value)
        .execute(&pool)
        .await
        .unwrap();
    }

    MIGRATOR.run(&pool).await.unwrap();

    let stored = |key: &'static str| {
        let pool = pool.clone();
        async move {
            sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = ?")
                .bind(key)
                .fetch_one(&pool)
                .await
                .unwrap()
        }
    };
    let rules: serde_json::Value = serde_json::from_str(&stored("rules").await).unwrap();
    assert_eq!(
        rules,
        serde_json::json!({
            "direct": {
                "kind": "one-to-one",
                "source": "whatsapp-main:123@s.whatsapp.net",
                "destination": "telegram-main:-100123#12",
                "priority": 0
            },
            "mirror": {
                "kind": "many-to-one",
                "source_connector": "whatsapp-main",
                "destination": "telegram-main:-100999",
                "except_chats": ["whatsapp-main:a%23b@g.us"],
                "priority": 1
            }
        })
    );
    // wuphf-storage cannot see the settings types; check the shape too.
    for entry in rules.as_object().unwrap().values() {
        let entry = entry.as_object().unwrap();
        assert!(entry.contains_key("kind") && entry.contains_key("priority"));
        for old in ["id", "flow", "source_chat", "destination_chat"] {
            assert!(!entry.contains_key(old), "{old} left behind");
        }
    }
    let notify: serde_json::Value = serde_json::from_str(&stored("notify").await).unwrap();
    assert_eq!(
        notify,
        serde_json::json!([
            "telegram-main:telegram:user:1#5",
            "whatsapp-main:x@s.whatsapp.net"
        ])
    );
    assert_eq!(stored("timezone").await, unrelated);
}

/// 0018 only adds a table: a database from before it keeps its settings, and
/// the new table takes a declaration with binary descriptor bytes intact.
/// (0020 replaces the table; see its own test.)
#[tokio::test]
async fn migration_0018_adds_connector_settings_declarations() {
    let pool = database_before(18).await;
    sqlx::query(
        "INSERT INTO settings (key, value, updated_by, updated_at_ms)
         VALUES ('notify', '[]', NULL, 1)",
    )
    .execute(&pool)
    .await
    .unwrap();
    migrator_before(20).run(&pool).await.unwrap();

    let descriptor = vec![0x0a_u8, 0x00, 0xff, 0x80];
    sqlx::query(
        "INSERT INTO connector_settings_declarations
           (connector_id, descriptor, json_schema, base_json, updated_at_ms)
         VALUES ('telegram-main', ?, '{}', '{}', 2)",
    )
    .bind(&descriptor)
    .execute(&pool)
    .await
    .unwrap();
    let row = sqlx::query("SELECT descriptor FROM connector_settings_declarations")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(row.get::<Vec<u8>, _>("descriptor"), descriptor);
    let kept = sqlx::query("SELECT value FROM settings WHERE key = 'notify'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(kept.get::<String, _>("value"), "[]");
}

/// 0019 only adds the chat profiles table: dispatches stored before it still
/// decode, and a stored profile decodes into today's model.
#[tokio::test]
async fn migration_0019_adds_chat_profiles() {
    let pool = database_before(19).await;
    insert_dispatches(&pool, &DISPATCHES_BEFORE_0019).await;

    MIGRATOR.run(&pool).await.unwrap();

    assert_dispatches(&pool).await;
    sqlx::query("INSERT INTO chat_profiles (connector_id, chat_id, payload) VALUES (?, ?, ?)")
        .bind("whatsapp-main")
        .bind("1@s.whatsapp.net")
        .bind(
            r#"{"connector_id":"whatsapp-main","chat_id":"1@s.whatsapp.net","disappearing":{"seconds":86400,"setting_timestamp":1700000000}}"#,
        )
        .execute(&pool)
        .await
        .unwrap();
    let profiles = decode_all::<ChatProfile>(&pool, "chat_profiles").await;
    assert_eq!(
        profiles,
        [ChatProfile {
            disappearing: Some(DisappearingTimer {
                seconds: 86_400,
                setting_timestamp: Some(1_700_000_000),
            }),
            ..ChatProfile::new(ConnectorId("whatsapp-main".into()), "1@s.whatsapp.net")
        }]
    );
}

/// 0020 moves connectors' declarations into the owner-keyed table: each row
/// becomes a connector's declaration, its descriptor bytes and base intact,
/// and plugins' declarations can be stored next to them.
#[tokio::test]
async fn migration_0020_keeps_connector_settings_declarations() {
    let pool = database_before(20).await;
    let descriptor = vec![0x0a_u8, 0x00, 0xff, 0x80];
    sqlx::query(
        "INSERT INTO connector_settings_declarations
           (connector_id, descriptor, json_schema, base_json, updated_at_ms)
         VALUES ('telegram-main', ?, '{\"type\":\"object\"}', '{\"log_level\":\"info\"}', 2)",
    )
    .bind(&descriptor)
    .execute(&pool)
    .await
    .unwrap();

    MIGRATOR.run(&pool).await.unwrap();

    sqlx::query(
        "INSERT INTO settings_declarations
           (owner_kind, owner_id, descriptor, json_schema, base_json, updated_at_ms)
         VALUES ('plugin', 'telegram-main', x'', '{}', '{}', 3)",
    )
    .execute(&pool)
    .await
    .unwrap();
    let rows = sqlx::query(
        "SELECT owner_kind, owner_id, descriptor, json_schema, base_json, updated_at_ms
         FROM settings_declarations ORDER BY owner_kind",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    let rows: Vec<_> = rows
        .iter()
        .map(|row| {
            (
                row.get::<String, _>("owner_kind"),
                row.get::<String, _>("owner_id"),
                row.get::<Vec<u8>, _>("descriptor"),
                row.get::<String, _>("json_schema"),
                row.get::<String, _>("base_json"),
                row.get::<i64, _>("updated_at_ms"),
            )
        })
        .collect();
    let owned = |kind: &str, id: &str| (kind.to_owned(), id.to_owned());
    assert_eq!(
        rows.iter()
            .map(|row| (row.0.clone(), row.1.clone()))
            .collect::<Vec<_>>(),
        [
            owned("connector", "telegram-main"),
            owned("plugin", "telegram-main")
        ]
    );
    assert_eq!(rows[0].2, descriptor);
    assert_eq!(rows[0].3, r#"{"type":"object"}"#);
    assert_eq!(rows[0].4, r#"{"log_level":"info"}"#);
    assert_eq!(rows[0].5, 2);
    let old = sqlx::query(
        "SELECT name FROM sqlite_master WHERE name = 'connector_settings_declarations'",
    )
    .fetch_optional(&pool)
    .await
    .unwrap();
    assert!(old.is_none(), "the old table is dropped");
}

/// 0021 keys event claims by connector. A claim made before it is attributed to
/// the connector whose received copy it produced, so that account still dedups
/// the event while a second account in the same group can claim it; a sent
/// copy under the same native id attributes nothing, and a claim with no
/// received copy is dropped. A platform without a variant of its own (stored
/// as an object) is matched too.
#[tokio::test]
async fn migration_0021_keys_seen_events_by_connector() {
    let pool = database_before(21).await;
    for (id, connector, platform) in [
        ("e-a", "whatsapp-a", r#""whats_app""#),
        ("e-b", "whatsapp-b", r#""whats_app""#),
        ("e-o", "custom-1", r#"{"other":"custom"}"#),
    ] {
        sqlx::query("INSERT INTO endpoints (id, payload) VALUES (?, ?)")
            .bind(id)
            .bind(format!(
                r#"{{"id":"{id}","connector_id":"{connector}","platform":{platform},"chat_id":"g@g.us","thread_id":null}}"#
            ))
            .execute(&pool)
            .await
            .unwrap();
    }
    for (message, endpoint, native, sent) in [
        ("m-1", "e-a", "n-1", 0),
        ("m-1", "e-b", "n-1", 1),
        ("m-2", "e-b", "n-2", 0),
        ("m-3", "e-o", "n-3", 0),
    ] {
        sqlx::query(
            "INSERT INTO message_copies (message_id, endpoint_id, platform_message_id, sent, payload)
             VALUES (?, ?, ?, ?, '{}')",
        )
        .bind(message)
        .bind(endpoint)
        .bind(native)
        .bind(sent)
        .execute(&pool)
        .await
        .unwrap();
    }
    for (platform, native) in [
        (r#""whats_app""#, "n-1"),
        (r#""whats_app""#, "n-2"),
        (r#"{"other":"custom"}"#, "n-3"),
        (r#""whats_app""#, "n-orphan"),
    ] {
        sqlx::query(
            "INSERT INTO seen_events (platform, chat_id, platform_message_id)
             VALUES (?, 'g@g.us', ?)",
        )
        .bind(platform)
        .bind(native)
        .execute(&pool)
        .await
        .unwrap();
    }

    MIGRATOR.run(&pool).await.unwrap();

    let rows = sqlx::query(
        "SELECT connector_id, chat_id, platform_message_id FROM seen_events
         ORDER BY platform_message_id",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    let claims = rows
        .iter()
        .map(|row| {
            (
                row.get::<String, _>("connector_id"),
                row.get::<String, _>("chat_id"),
                row.get::<String, _>("platform_message_id"),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        claims,
        [
            ("whatsapp-a".into(), "g@g.us".into(), "n-1".into()),
            ("whatsapp-b".into(), "g@g.us".into(), "n-2".into()),
            ("custom-1".into(), "g@g.us".into(), "n-3".into()),
        ]
    );
}

/// 0022 only adds the health tables: a database from before it keeps its
/// dispatches, and the new tables take a problem, an ignore and a held
/// dispatch.
#[tokio::test]
async fn migration_0022_adds_the_health_tables() {
    let pool = database_before(22).await;
    insert_dispatches(&pool, &DISPATCHES_BEFORE_0019).await;

    MIGRATOR.run(&pool).await.unwrap();

    assert_dispatches(&pool).await;
    for statement in [
        "INSERT INTO health_problems (key, since_ms, payload) VALUES ('k', 1, '{}')",
        "INSERT INTO health_ignores (key) VALUES ('k')",
        "INSERT INTO held_dispatches (id, pause, held_at_ms, payload) VALUES ('d', '{}', 1, '{}')",
    ] {
        sqlx::query(statement).execute(&pool).await.unwrap();
    }
}

/// 0023 only adds the reactions table: a database from before it keeps its
/// dispatches, and the table takes one reaction per copy and reactor.
#[tokio::test]
async fn migration_0023_adds_the_reactions_table() {
    let pool = database_before(23).await;
    insert_dispatches(&pool, &DISPATCHES_BEFORE_0019).await;

    MIGRATOR.run(&pool).await.unwrap();

    assert_dispatches(&pool).await;
    let insert = "INSERT INTO reactions (message_id, endpoint_id, reactor, emoji, updated_at_ms)
                  VALUES ('m', 'e', 'r', '👍', 1)";
    sqlx::query(insert).execute(&pool).await.unwrap();
    assert!(sqlx::query(insert).execute(&pool).await.is_err());
}

/// An observed row stored before `sent_by` existed decodes with no client, and
/// today's model writes it back byte for byte: the key appears only when set.
#[test]
fn observed_rows_without_sent_by_keep_their_json() {
    let stored = serde_json::to_string(&sample_observed()).unwrap();
    assert!(!stored.contains("sent_by"), "{stored}");
    let decoded: ObservedMessage = serde_json::from_str(&stored).unwrap();
    assert_eq!(decoded.sent_by, None);
    assert_eq!(serde_json::to_string(&decoded).unwrap(), stored);

    let tagged = ObservedMessage {
        sent_by: Some("wuphf-tui".into()),
        ..sample_observed()
    };
    let json = serde_json::to_string(&tagged).unwrap();
    assert!(json.ends_with(r#","sent_by":"wuphf-tui"}"#), "{json}");
    assert_eq!(
        serde_json::from_str::<ObservedMessage>(&json).unwrap(),
        tagged
    );
}

/// A profile 045 stored (a timer only) decodes with 046's fields unknown, and
/// today's model writes it back byte for byte: no new key appears while empty.
/// The same holds for conversations without `introduced` (see the 0014 test,
/// whose stored rows compare byte for byte with `introduced: false`).
#[tokio::test]
async fn chat_profiles_from_045_decode() {
    let pool = database_before(20).await;
    let stored =
        r#"{"connector_id":"whatsapp-main","chat_id":"1@g.us","disappearing":{"seconds":604800}}"#;
    sqlx::query("INSERT INTO chat_profiles (connector_id, chat_id, payload) VALUES (?, ?, ?)")
        .bind("whatsapp-main")
        .bind("1@g.us")
        .bind(stored)
        .execute(&pool)
        .await
        .unwrap();

    let expected = ChatProfile {
        disappearing: Some(DisappearingTimer::new(604_800)),
        ..ChatProfile::new(ConnectorId("whatsapp-main".into()), "1@g.us")
    };
    assert_eq!(expected.title, None);
    assert_eq!(expected.avatar, None);
    assert_stored(&pool, "chat_profiles", "chat_id", "1@g.us", &expected).await;
}

/// A profile 046 stored (title, description, avatar, count) decodes with
/// 047's pin, mute and archive unknown, and is written back byte for byte.
#[tokio::test]
async fn chat_profiles_from_046_decode() {
    let pool = database_before(20).await;
    let media = MediaId(Uuid::from_u128(77));
    let stored = format!(
        r#"{{"connector_id":"whatsapp-main","chat_id":"1@g.us","title":"[G] Family","description":"Weekend plans","avatar":{{"id":"pic-1","media_id":"{media}","hash":"ab"}},"participant_count":12}}"#
    );
    sqlx::query("INSERT INTO chat_profiles (connector_id, chat_id, payload) VALUES (?, ?, ?)")
        .bind("whatsapp-main")
        .bind("1@g.us")
        .bind(&stored)
        .execute(&pool)
        .await
        .unwrap();

    let expected = ChatProfile {
        title: Some("[G] Family".into()),
        description: Some("Weekend plans".into()),
        avatar: Some(wuphf_model::ChatAvatar {
            id: "pic-1".into(),
            media_id: Some(media),
            hash: Some("ab".into()),
        }),
        participant_count: Some(12),
        ..ChatProfile::new(ConnectorId("whatsapp-main".into()), "1@g.us")
    };
    assert_eq!(expected.pinned, None);
    assert_eq!(expected.mute, None);
    assert_eq!(expected.archived, None);
    assert_stored(&pool, "chat_profiles", "chat_id", "1@g.us", &expected).await;
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
    decode_all::<ChatProfile>(&pool, "chat_profiles").await;

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
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!("SELECT payload FROM {table}")))
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
                chat_id: None,
            }),
            disappearing: None,
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
                by_owner: false,
                target_author: MessageAuthor::default(),
                confirm: None,
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
        sent_by: None,
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

/// The sample dispatches as the model wrote them before 0019, captured with
/// `print_current_json`. Later fields are additive and absent when unset, so
/// these rows must still be stored exactly as today's model writes them.
const DISPATCHES_BEFORE_0019: [&str; 7] = [
    r#"{"id":"00000000-0000-0000-0000-000000000064","state":{"Acknowledged":{"native_id":"tg-1"}},"to":{"endpoint_id":"00000000-0000-0000-0000-000000000002","connector_id":"telegram","address":{"chat_id":"-100","thread_id":"7"},"create_thread":"Alice"},"conversation_id":"00000000-0000-0000-0000-000000000003","action":{"Send":{"message":{"id":"00000000-0000-0000-0000-000000000001","conversation_id":"00000000-0000-0000-0000-000000000003","author_id":"15550001111@s.whatsapp.net","participant":"15550001111@s.whatsapp.net","author_name":"Alice","text":"hello","attachments":[{"media_id":"00000000-0000-0000-0000-000000000004","kind":"image","file_name":"photo.jpg","mime_type":"image/jpeg","size_bytes":1234,"animated":false}],"reply_to":"00000000-0000-0000-0000-000000000005","timestamp_ms":1000,"deleted":false},"reply":{"message_id":"00000000-0000-0000-0000-000000000005","native_id":"tg-parent","author_id":"15550002222@s.whatsapp.net","participant":null,"text":"parent","attachments":[],"authored_by_bridge":true}}}}"#,
    r#"{"id":"00000000-0000-0000-0000-000000000065","state":"Pending","to":{"endpoint_id":"00000000-0000-0000-0000-000000000002","connector_id":"telegram","address":{"chat_id":"-100","thread_id":"7"},"create_thread":null},"conversation_id":"00000000-0000-0000-0000-000000000003","action":{"Edit":{"message":{"id":"00000000-0000-0000-0000-000000000001","conversation_id":"00000000-0000-0000-0000-000000000003","author_id":"15550001111@s.whatsapp.net","participant":"15550001111@s.whatsapp.net","author_name":"Alice","text":"hello","attachments":[{"media_id":"00000000-0000-0000-0000-000000000004","kind":"image","file_name":"photo.jpg","mime_type":"image/jpeg","size_bytes":1234,"animated":false}],"reply_to":"00000000-0000-0000-0000-000000000005","timestamp_ms":1000,"deleted":false},"native_id":"tg-1"}}}"#,
    r#"{"id":"00000000-0000-0000-0000-000000000066","state":"Pending","to":{"endpoint_id":"00000000-0000-0000-0000-000000000002","connector_id":"telegram","address":{"chat_id":"-100","thread_id":"7"},"create_thread":null},"conversation_id":"00000000-0000-0000-0000-000000000003","action":{"Delete":{"message_id":"00000000-0000-0000-0000-000000000001","native_id":"tg-1"}}}"#,
    r#"{"id":"00000000-0000-0000-0000-000000000067","state":"Pending","to":{"endpoint_id":"00000000-0000-0000-0000-000000000002","connector_id":"telegram","address":{"chat_id":"-100","thread_id":"7"},"create_thread":null},"conversation_id":"00000000-0000-0000-0000-000000000003","action":{"RenameThread":{"title":"Alice (renamed)"}}}"#,
    r#"{"id":"00000000-0000-0000-0000-000000000068","state":"Pending","to":{"endpoint_id":"00000000-0000-0000-0000-000000000002","connector_id":"telegram","address":{"chat_id":"-100","thread_id":"7"},"create_thread":null},"conversation_id":"00000000-0000-0000-0000-000000000003","action":{"React":{"message_id":"00000000-0000-0000-0000-000000000001","native_id":"tg-1","reactor":"15550002222@s.whatsapp.net","reactor_name":"Bob","emoji":"👍"}}}"#,
    r#"{"id":"00000000-0000-0000-0000-000000000069","state":"Pending","to":{"endpoint_id":"00000000-0000-0000-0000-000000000002","connector_id":"telegram","address":{"chat_id":"-100","thread_id":"7"},"create_thread":null},"conversation_id":"00000000-0000-0000-0000-000000000003","action":{"Receipt":{"message_id":"00000000-0000-0000-0000-000000000001","native_id":"wa-1","kind":"delivered"}}}"#,
    r#"{"id":"00000000-0000-0000-0000-00000000006a","state":"Pending","to":{"endpoint_id":"00000000-0000-0000-0000-000000000002","connector_id":"telegram","address":{"chat_id":"-100","thread_id":"7"},"create_thread":null},"conversation_id":"00000000-0000-0000-0000-000000000003","action":{"Receipt":{"message_id":"00000000-0000-0000-0000-000000000001","native_id":"wa-1","kind":"read"}}}"#,
];

/// 0024 only adds the story expiries table: a database from before it keeps
/// its dispatches, and the table takes one expiry per message.
#[tokio::test]
async fn migration_0024_adds_the_message_expiries_table() {
    let pool = database_before(24).await;
    insert_dispatches(&pool, &DISPATCHES_BEFORE_0019).await;

    MIGRATOR.run(&pool).await.unwrap();

    assert_dispatches(&pool).await;
    let insert = "INSERT INTO message_expiries (message_id, expires_at_ms) VALUES ('m', 1)";
    sqlx::query(insert).execute(&pool).await.unwrap();
    assert!(sqlx::query(insert).execute(&pool).await.is_err());
}

/// 0025 only adds the reaction summaries table (plan 085): a database from
/// before it keeps its dispatches, and the table keeps one row per message.
#[tokio::test]
async fn migration_0025_adds_the_reaction_summaries_table() {
    let pool = database_before(25).await;
    insert_dispatches(&pool, &DISPATCHES_BEFORE_0019).await;

    MIGRATOR.run(&pool).await.unwrap();

    assert_dispatches(&pool).await;
    let insert = "INSERT INTO reaction_summaries (message_id, endpoint_id, counts, updated_at_ms) \
                  VALUES ('m', 'e', '[]', 1)";
    sqlx::query(insert).execute(&pool).await.unwrap();
    assert!(sqlx::query(insert).execute(&pool).await.is_err());
}
