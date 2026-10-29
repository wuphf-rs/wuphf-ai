use super::*;
use botconf_panel::Names;
use std::path::PathBuf;
use wuphf_model::{
    AddressLink, Alias, Conversation, ConversationId, EndpointId, ObservedDirection,
    ObservedMessage, Platform, RuleId,
};
use wuphf_settings::{WuphfSchema, base, load};
use wuphf_storage::InMemoryStorage;

const TELEGRAM_TOKEN: &str = "secret-TELEGRAM_TOKEN";
const WHATSAPP_TOKEN: &str = "secret-WHATSAPP_TOKEN";
const OBSERVER_TOKEN: &str = "observer-secret";

/// Two connectors, each with one owner, and one rule.
const CONFIG: &str = r#"
listen = "127.0.0.1:50051"
database_url = "sqlite://unused.sqlite"

[[connectors]]
id = "telegram-main"
platform = "telegram"
token_env = "TELEGRAM_TOKEN"
owner_ids = ["704338780"]

[[connectors]]
id = "whatsapp-main"
platform = "whatsapp"
token_env = "WHATSAPP_TOKEN"
owner_ids = ["98765@lid"]

[rules.wa-to-telegram]
kind = "many-to-one"
source_connector = "whatsapp-main"
destination = "telegram-main:telegram:channel:1"
"#;

/// A config file in the temp directory.
fn config_file(toml: &str) -> PathBuf {
    let path =
        std::env::temp_dir().join(format!("wuphf-settings-auth-{}.toml", uuid::Uuid::new_v4()));
    std::fs::write(&path, toml).unwrap();
    path
}

/// The settings store over `CONFIG`, every token variable `secret-<name>`.
async fn store() -> Arc<SettingsStore> {
    let schema = WuphfSchema::with_env(|name| Some(format!("secret-{name}")));
    let path = config_file(CONFIG);
    let store = load(schema, base(&path).unwrap(), botconf::MemoryStorage::new())
        .await
        .unwrap();
    Arc::new(store)
}

async fn authorizer(observer_token: Option<&str>) -> WuphfAuthorizer {
    let store = store().await;
    let tokens = ConnectorTokens::new(store.current().derived.connector_tokens.clone());
    WuphfAuthorizer::new(store, Arc::new(tokens), observer_token.map(Into::into))
}

fn metadata(token: Option<&str>) -> MetadataMap {
    let mut metadata = MetadataMap::new();
    if let Some(token) = token {
        metadata.insert("authorization", format!("Bearer {token}").parse().unwrap());
    }
    metadata
}

fn actor(id: &str) -> Actor {
    Actor {
        domain: "telegram.user".into(),
        id: id.into(),
    }
}

const CHANGE: Operation<'static> = Operation::Change { key: "notify" };

fn code(result: Result<Principal, Status>) -> tonic::Code {
    result.expect_err("the request is refused").code()
}

#[tokio::test]
async fn missing_token_is_unauthenticated() {
    let authorizer = authorizer(Some(OBSERVER_TOKEN)).await;
    let result = authorizer.authorize(&metadata(None), Some(&actor("704338780")), CHANGE);
    assert_eq!(code(result), tonic::Code::Unauthenticated);
}

#[tokio::test]
async fn unknown_token_is_unauthenticated() {
    let authorizer = authorizer(Some(OBSERVER_TOKEN)).await;
    let result = authorizer.authorize(&metadata(Some("nope")), Some(&actor("704338780")), CHANGE);
    assert_eq!(code(result), tonic::Code::Unauthenticated);
}

#[tokio::test]
async fn observer_token_acts_as_owner() {
    let authorizer = authorizer(Some(OBSERVER_TOKEN)).await;
    for actor in [None, Some(actor("a stranger"))] {
        let principal = authorizer
            .authorize(&metadata(Some(OBSERVER_TOKEN)), actor.as_ref(), CHANGE)
            .unwrap();
        assert_eq!(principal.actor.as_deref(), Some("observer"));
    }
}

#[tokio::test]
async fn observer_credential_refused_when_no_observer_token() {
    for configured in [None, Some("")] {
        let authorizer = authorizer(configured).await;
        for presented in [Some(""), Some(OBSERVER_TOKEN)] {
            let result = authorizer.authorize(&metadata(presented), None, CHANGE);
            assert_eq!(
                code(result),
                tonic::Code::Unauthenticated,
                "configured {configured:?}, presented {presented:?}"
            );
        }
    }
}

#[tokio::test]
async fn connector_without_actor_is_denied() {
    let authorizer = authorizer(None).await;
    for actor in [None, Some(actor(""))] {
        let result = authorizer.authorize(&metadata(Some(TELEGRAM_TOKEN)), actor.as_ref(), CHANGE);
        assert_eq!(code(result), tonic::Code::PermissionDenied);
    }
}

#[tokio::test]
async fn connector_acting_for_its_owner_is_allowed() {
    let authorizer = authorizer(None).await;
    let principal = authorizer
        .authorize(
            &metadata(Some(TELEGRAM_TOKEN)),
            Some(&actor("704338780")),
            CHANGE,
        )
        .unwrap();
    assert_eq!(principal.actor.as_deref(), Some("telegram-main:704338780"));
}

#[tokio::test]
async fn connector_acting_for_a_stranger_is_denied() {
    let authorizer = authorizer(None).await;
    let result = authorizer.authorize(&metadata(Some(TELEGRAM_TOKEN)), Some(&actor("111")), CHANGE);
    assert_eq!(code(result), tonic::Code::PermissionDenied);
}

#[tokio::test]
async fn connector_cannot_act_for_another_connectors_owner() {
    let authorizer = authorizer(None).await;
    // The WhatsApp owner, claimed by the Telegram connector.
    let result = authorizer.authorize(
        &metadata(Some(TELEGRAM_TOKEN)),
        Some(&actor("98765@lid")),
        CHANGE,
    );
    assert_eq!(code(result), tonic::Code::PermissionDenied);
    // The same owner through their own connector.
    let principal = authorizer
        .authorize(
            &metadata(Some(WHATSAPP_TOKEN)),
            Some(&actor("98765@lid")),
            CHANGE,
        )
        .unwrap();
    assert_eq!(principal.actor.as_deref(), Some("whatsapp-main:98765@lid"));
}

// ---- names ----

fn whatsapp() -> ConnectorId {
    ConnectorId("whatsapp-main".into())
}

fn observed(chat_id: &str, native_id: &str, timestamp_ms: i64) -> ObservedMessage {
    ObservedMessage {
        connector_id: whatsapp(),
        platform: Platform::WhatsApp,
        chat_id: chat_id.into(),
        thread_id: None,
        native_id: native_id.into(),
        author_id: "sender".into(),
        author_name: None,
        participant: None,
        text: Some("hi".into()),
        attachments: vec![],
        reply_to_native_id: None,
        direction: ObservedDirection::Incoming,
        timestamp_ms,
        deleted: false,
        edited: false,
    }
}

async fn names() -> (WuphfNames<InMemoryStorage>, Arc<InMemoryStorage>) {
    let storage = Arc::new(InMemoryStorage::default());
    (
        WuphfNames::new(Arc::clone(&storage), store().await),
        storage,
    )
}

async fn chat_name(names: &WuphfNames<InMemoryStorage>, id: &str) -> Option<String> {
    names
        .name(domains::CHAT, &Value::String(id.into()))
        .await
        .map(|name| name.full)
}

#[tokio::test]
async fn chat_names_come_from_the_observation_log() {
    let (names, storage) = names().await;
    storage
        .log_observed(observed("120363@g.us", "m1", 1), Some("Family".into()))
        .await
        .unwrap();
    assert_eq!(
        chat_name(&names, "whatsapp-main:120363@g.us")
            .await
            .as_deref(),
        Some("Family")
    );
}

#[tokio::test]
async fn chat_names_follow_linked_addresses() {
    let (names, storage) = names().await;
    storage
        .log_observed(observed("98765@lid", "m1", 1), Some("Mom".into()))
        .await
        .unwrap();
    storage
        .put_address_link(AddressLink {
            connector_id: whatsapp(),
            address: ChatAddress::new("12345@s.whatsapp.net", None),
            linked_address: ChatAddress::new("98765@lid", None),
        })
        .await
        .unwrap();
    assert_eq!(
        chat_name(&names, "whatsapp-main:12345@s.whatsapp.net")
            .await
            .as_deref(),
        Some("Mom")
    );
}

#[tokio::test]
async fn thread_names_come_from_their_conversation() {
    let (names, storage) = names().await;
    let telegram = ConnectorId("telegram-main".into());
    let thread = ChatAddress::new("telegram:channel:1", Some("7".into()));
    let endpoint = EndpointId::new();
    storage
        .put_alias(Alias {
            connector_id: telegram.clone(),
            address: thread,
            endpoint_id: endpoint,
            is_canonical: true,
        })
        .await
        .unwrap();
    storage
        .put_conversation(Conversation {
            id: ConversationId::new(),
            rule_id: Some(RuleId("wa-to-telegram".into())),
            source: ChatRef {
                connector_id: whatsapp(),
                address: ChatAddress::new("120363@g.us", None),
            },
            endpoints: vec![endpoint],
            title: Some("Family".into()),
        })
        .await
        .unwrap();

    let name = chat_name(&names, "telegram-main:telegram:channel:1#7").await;
    assert_eq!(name.as_deref(), Some("Family"));

    // With the container's own title known too, both parts show.
    storage
        .log_observed(
            ObservedMessage {
                connector_id: telegram,
                chat_id: "telegram:channel:1".into(),
                platform: Platform::Telegram,
                ..observed("telegram:channel:1", "t1", 2)
            },
            Some("Mirrors".into()),
        )
        .await
        .unwrap();
    let name = chat_name(&names, "telegram-main:telegram:channel:1#7").await;
    assert_eq!(name.as_deref(), Some("Mirrors › Family"));
}

#[tokio::test]
async fn unknown_chats_have_no_name() {
    let (names, _) = names().await;
    assert_eq!(chat_name(&names, "whatsapp-main:unseen@g.us").await, None);
    assert_eq!(chat_name(&names, "not a chat").await, None);
    assert!(names.name(domains::CHAT, &Value::from(42)).await.is_none());
}

#[tokio::test]
async fn chat_candidates_are_recent_and_filtered() {
    let (names, storage) = names().await;
    for (chat, title, ts) in [
        ("old@g.us", "Old friends", 1),
        ("family@g.us", "Family", 3),
        ("work@g.us", "Work", 2),
    ] {
        storage
            .log_observed(observed(chat, chat, ts), Some(title.into()))
            .await
            .unwrap();
    }
    // A chat on a connector the configuration no longer has.
    storage
        .log_observed(
            ObservedMessage {
                connector_id: ConnectorId("gone".into()),
                ..observed("gone@g.us", "g1", 9)
            },
            Some("Gone".into()),
        )
        .await
        .unwrap();

    let ids = |candidates: Vec<(Value, Name)>| -> Vec<String> {
        candidates
            .into_iter()
            .map(|(id, _)| id.as_str().unwrap().to_owned())
            .collect()
    };
    assert_eq!(
        ids(names.candidates(domains::CHAT, "", 10).await),
        [
            "whatsapp-main:family@g.us",
            "whatsapp-main:work@g.us",
            "whatsapp-main:old@g.us"
        ]
    );
    let family = names.candidates(domains::CHAT, "fam", 10).await;
    assert_eq!(family.len(), 1);
    assert_eq!(family[0].1.short, "Family");
    assert_eq!(names.candidates(domains::CHAT, "", 1).await.len(), 1);
}

#[tokio::test]
async fn connectors_and_rules_are_named_and_offered() {
    let (names, _) = names().await;
    let name = names
        .name(domains::CONNECTOR, &Value::String("telegram-main".into()))
        .await
        .unwrap();
    assert_eq!(name.full, "telegram-main (telegram)");
    assert!(
        names
            .name(domains::CONNECTOR, &Value::String("gone".into()))
            .await
            .is_none()
    );
    let connectors: Vec<_> = names
        .candidates(domains::CONNECTOR, "", 10)
        .await
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    assert_eq!(
        connectors,
        [
            Value::String("telegram-main".into()),
            Value::String("whatsapp-main".into())
        ]
    );
    let rules: Vec<_> = names
        .candidates(domains::RULE, "wa", 10)
        .await
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    assert_eq!(rules, [Value::String("wa-to-telegram".into())]);
}
