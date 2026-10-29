//! Loading and deriving the core configuration. Every test that reads the
//! environment runs inside a `figment::Jail`, which serializes them and
//! restores the environment afterwards.
#![allow(clippy::result_large_err)] // `figment::Jail` returns `figment::Error`.

use crate::{
    Configuration, LiveRouting, LoadError, SettingsStorage, SettingsStore, WuphfSchema, base, load,
};
use botconf::{MemoryStorage, SettingsError, Source, Storage, StoredOverride};
use figment::Jail;
use std::path::Path;
use std::sync::Arc;
use wuphf_core::RoutingSource;
use wuphf_model::{AddressSet, ChatAddress, ChatFilter, ConnectorId, RuleId, RuleKind};
use wuphf_storage::{SettingsOverrides, SqliteStorage};

const EXAMPLE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../config/example.toml");

const MINIMAL: &str = r#"
listen = "127.0.0.1:50051"
database_url = "sqlite://test.sqlite"
"#;

/// Every token variable resolves to `secret-<name>`.
fn every_token_set() -> WuphfSchema {
    WuphfSchema::with_env(|name| Some(format!("secret-{name}")))
}

/// Writes `toml` to a file in the jail and extracts it through [`base`].
fn extract(jail: &mut Jail, toml: &str) -> figment::Result<Configuration> {
    jail.create_file("wuphf.toml", toml)?;
    base(Path::new("wuphf.toml")).unwrap().extract()
}

/// Loads the settings store from `path` (synchronously, for use in a jail).
fn store(schema: WuphfSchema, path: &Path) -> Result<SettingsStore, SettingsError> {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(load(schema, base(path).unwrap(), MemoryStorage::new()))
}

/// The shipped example config is the reference for the rule format; it must
/// always parse, and its flows must land in the expected variants.
#[test]
fn example_configuration_parses() {
    Jail::expect_with(|_| {
        let configuration: Configuration = base(Path::new(EXAMPLE)).unwrap().extract()?;
        assert_eq!(configuration.rules.len(), 2);
        for (id, entry) in &configuration.rules {
            let RuleKind::ManyToOne {
                source_chats_filter,
                destination_connector,
                destination_chat,
                ..
            } = entry.flow.clone().into_rule_kind(id).unwrap()
            else {
                panic!("example rules are many-to-one mirrors");
            };
            assert_eq!(source_chats_filter, ChatFilter::All);
            assert_eq!(destination_connector, ConnectorId("telegram-main".into()));
            assert_eq!(destination_chat.chat_id, "telegram:channel:2060340733");
        }
        assert_eq!(configuration.notify.len(), 1);
        let target = &configuration.notify[0].0;
        assert_eq!(target.connector_id, ConnectorId("telegram-main".into()));
        assert_eq!(target.address.chat_id, "telegram:user:704338780");
        assert_eq!(target.address.thread_id, None);
        Ok(())
    });
}

fn chat(chat_id: &str, thread_id: Option<&str>) -> ChatAddress {
    ChatAddress::new(chat_id, thread_id.map(Into::into))
}

#[test]
fn one_to_one_rule_and_filters_parse() {
    Jail::expect_with(|jail| {
        let configuration = extract(
            jail,
            &format!(
                r#"{MINIMAL}
                [rules.direct]
                kind = "one-to-one"
                source = "whatsapp-main:123@s.whatsapp.net"
                destination = "telegram-main:-100123#12"

                [rules.filtered-mirror]
                kind = "many-to-one"
                source_connector = "whatsapp-main"
                destination = "telegram-main:-100999"
                except_chats = ["whatsapp-main:spam@g.us"]

                [rules.only-mirror]
                kind = "many-to-one"
                source_connector = "whatsapp-main"
                destination = "telegram-main:-100999"
                only_chats = ["whatsapp-main:family@g.us", "whatsapp-main:a%23b#7"]
                "#
            ),
        )?;
        let kind = |id: &str| {
            configuration.rules[id]
                .flow
                .clone()
                .into_rule_kind(id)
                .unwrap()
        };
        let whatsapp = || ConnectorId("whatsapp-main".into());
        let telegram = || ConnectorId("telegram-main".into());
        assert_eq!(
            kind("direct"),
            RuleKind::OneToOne {
                source_connector: whatsapp(),
                source_chat: chat("123@s.whatsapp.net", None),
                destination_connector: telegram(),
                destination_chat: chat("-100123", Some("12")),
            }
        );
        assert_eq!(
            kind("filtered-mirror"),
            RuleKind::ManyToOne {
                source_connector: whatsapp(),
                source_chats_filter: ChatFilter::Not {
                    chats: vec![chat("spam@g.us", None)],
                },
                destination_connector: telegram(),
                destination_chat: chat("-100999", None),
            }
        );
        assert_eq!(
            kind("only-mirror"),
            RuleKind::ManyToOne {
                source_connector: whatsapp(),
                source_chats_filter: ChatFilter::Only {
                    chats: vec![chat("family@g.us", None), chat("a#b", Some("7"))],
                },
                destination_connector: telegram(),
                destination_chat: chat("-100999", None),
            }
        );
        Ok(())
    });
}

#[test]
fn functions_section_parses() {
    Jail::expect_with(|jail| {
        let configuration = extract(
            jail,
            &format!(
                r#"{MINIMAL}
                [functions]
                default_deadline_ms = 30000
                max_deadline_ms = 60000
                invoke_token_env = "WUPHF_INVOKE_TOKEN"

                [[functions.providers]]
                id = "mediaconv"
                token_env = "WUPHF_PROVIDER_MEDIACONV_TOKEN"
                functions = ["media.convert"]

                [functions.overrides."media.convert"]
                default_deadline_ms = 20000
                "#
            ),
        )?;
        let functions = &configuration.functions;
        assert_eq!(functions.providers.len(), 1);
        assert_eq!(functions.providers[0].id, "mediaconv");
        assert_eq!(
            functions.overrides["media.convert"].default_deadline_ms,
            Some(20_000)
        );
        // Unset fields keep their defaults.
        assert_eq!(functions.max_queued_calls, 256);
        Ok(())
    });
}

#[test]
fn connector_without_token_env_is_rejected() {
    Jail::expect_with(|jail| {
        let error = extract(
            jail,
            &format!(
                r#"{MINIMAL}
                [[connectors]]
                id = "telegram-main"
                platform = "telegram"
                "#
            ),
        )
        .expect_err("a connector needs token_env");
        assert!(error.to_string().contains("token_env"), "{error}");
        Ok(())
    });
}

#[test]
fn connector_tokens_are_read_from_the_environment() {
    Jail::expect_with(|_| {
        let example = Path::new(EXAMPLE);
        let store = store(every_token_set(), example).unwrap();
        assert_eq!(
            store.current().derived.connector_tokens[&ConnectorId("telegram-main".into())],
            "secret-WUPHF_CONNECTOR_TELEGRAM_MAIN_TOKEN"
        );

        let unset = WuphfSchema::with_env(|name| {
            (name != "WUPHF_CONNECTOR_TELEGRAM_MAIN_TOKEN").then(|| "x".to_owned())
        });
        let error = store_error(unset, example);
        assert!(
            error.contains(
                "connector telegram-main reads its token from \
                 WUPHF_CONNECTOR_TELEGRAM_MAIN_TOKEN, which is unset or empty"
            ),
            "{error}"
        );

        let empty = WuphfSchema::with_env(|name| {
            Some(if name == "WUPHF_CONNECTOR_TELEGRAM_MAIN_TOKEN" {
                String::new()
            } else {
                "x".to_owned()
            })
        });
        let error = store_error(empty, example);
        assert!(
            error.contains("connector telegram-main reads its token"),
            "an empty token fails startup: {error}"
        );
        Ok(())
    });
}

/// Two connectors; `owners` is spliced into the first one's table.
fn two_connectors(owners: &str) -> String {
    format!(
        r#"{MINIMAL}
        [[connectors]]
        id = "telegram-main"
        platform = "telegram"
        token_env = "TELEGRAM_TOKEN"
        {owners}

        [[connectors]]
        id = "whatsapp-main"
        platform = "whatsapp"
        token_env = "WHATSAPP_TOKEN"
        "#
    )
}

#[test]
fn connector_owner_ids_parse() {
    Jail::expect_with(|jail| {
        jail.create_file(
            "wuphf.toml",
            &two_connectors(r#"owner_ids = ["704338780"]"#),
        )?;
        let store = store(every_token_set(), Path::new("wuphf.toml")).unwrap();
        let owners = &store.current().derived.owners;
        assert_eq!(
            owners[&ConnectorId("telegram-main".into())],
            ["704338780".to_owned()].into_iter().collect()
        );
        assert!(owners[&ConnectorId("whatsapp-main".into())].is_empty());
        Ok(())
    });
}

#[test]
fn owner_ids_must_not_be_blank() {
    Jail::expect_with(|jail| {
        jail.create_file("wuphf.toml", &two_connectors(r#"owner_ids = [" "]"#))?;
        let error = store_error(every_token_set(), Path::new("wuphf.toml"));
        assert!(
            error.contains("connector telegram-main: owner_ids must not contain a blank id"),
            "{error}"
        );

        jail.create_file("wuphf.toml", &two_connectors(r#"owner_ids = ["1", "1"]"#))?;
        let error = store_error(every_token_set(), Path::new("wuphf.toml"));
        assert!(error.contains("owner id \"1\" is listed twice"), "{error}");
        Ok(())
    });
}

#[test]
fn owner_ids_are_not_runtime_keys() {
    Jail::expect_with(|jail| {
        jail.create_file(
            "wuphf.toml",
            &two_connectors(r#"owner_ids = ["704338780"]"#),
        )?;
        let store = store(every_token_set(), Path::new("wuphf.toml")).unwrap();
        runtime().block_on(async {
            for key in ["connectors", "connectors.0.owner_ids", "owner_ids"] {
                let error = store
                    .set(key, serde_json::json!(["intruder"]), None)
                    .await
                    .unwrap_err();
                assert!(
                    matches!(error, SettingsError::UnknownKey(_)),
                    "{key}: {error}"
                );
            }
        });
        Ok(())
    });
}

#[test]
fn environment_overrides_the_file() {
    Jail::expect_with(|jail| {
        jail.set_env("WUPHF_CORE_LISTEN", "127.0.0.1:6000");
        jail.set_env("WUPHF_CORE_FUNCTIONS__MAX_DEADLINE_MS", "1234");
        let configuration = extract(jail, MINIMAL)?;
        assert_eq!(configuration.listen.to_string(), "127.0.0.1:6000");
        assert_eq!(configuration.functions.max_deadline_ms, 1234);
        Ok(())
    });
}

/// The core's environment also holds tokens and connector variables; only
/// `WUPHF_CORE_*` may be read as configuration.
#[test]
fn unrelated_wuphf_variables_are_not_read() {
    Jail::expect_with(|jail| {
        jail.set_env("WUPHF_INVOKE_TOKEN", "x");
        jail.set_env("WUPHF_LOG_LEVEL", "debug");
        jail.create_file("wuphf.toml", MINIMAL)?;
        let figment = base(Path::new("wuphf.toml")).unwrap();
        for key in ["invoke_token", "log_level", "core_log_level"] {
            assert!(figment.find_value(key).is_err(), "{key} was read");
        }
        let configuration: Configuration = figment.extract()?;
        assert_eq!(configuration.listen.to_string(), "127.0.0.1:50051");
        Ok(())
    });
}

#[test]
fn observer_token_is_redacted_when_serialized() {
    Jail::expect_with(|jail| {
        let configuration = extract(jail, &format!("{MINIMAL}observer_token = \"s3cret\"\n"))?;
        let serialized = serde_json::to_string(&configuration).unwrap();
        assert!(!serialized.contains("s3cret"), "{serialized}");
        assert!(serialized.contains("<redacted>"), "{serialized}");
        assert!(!format!("{configuration:?}").contains("s3cret"));
        assert_eq!(configuration.observer_token.unwrap().expose(), "s3cret");
        Ok(())
    });
}

#[test]
fn store_loads_the_example_and_derives_routing() {
    Jail::expect_with(|_| {
        let store = store(every_token_set(), Path::new(EXAMPLE)).unwrap();
        let snapshot = store.current();
        let configuration: Configuration = base(Path::new(EXAMPLE)).unwrap().extract()?;
        assert_eq!(
            snapshot.derived.connectors.len(),
            configuration.connectors.len()
        );
        assert_eq!(snapshot.derived.connector_tokens.len(), 4);
        assert!(
            snapshot
                .derived
                .routing
                .rule(&RuleId("wa-to-telegram-mirror".into()))
                .is_some()
        );
        assert_eq!(snapshot.derived.functions.provider_tokens().count(), 1);
        Ok(())
    });
}

/// The example's rules and notice targets name only configured connectors.
#[test]
fn example_configuration_validates() {
    Jail::expect_with(|_| {
        let store = store(every_token_set(), Path::new(EXAMPLE)).unwrap();
        assert_eq!(store.current().derived.routing.validate(), Ok(()));
        Ok(())
    });
}

#[test]
fn derive_rejects_a_rule_on_an_unknown_connector() {
    Jail::expect_with(|jail| {
        jail.create_file(
            "wuphf.toml",
            &format!(
                r#"{MINIMAL}
                [rules.nowhere]
                kind = "many-to-one"
                source_connector = "whatsapp-main"
                destination = "telegram-main:-100123"
                "#
            ),
        )?;
        let error = store_error(every_token_set(), Path::new("wuphf.toml"));
        assert!(
            error.contains("rule nowhere names connector whatsapp-main"),
            "{error}"
        );
        Ok(())
    });
}

/// Two connectors to route between, for tests that derive rules.
const CONNECTORS: &str = r#"
[[connectors]]
id = "whatsapp-main"
platform = "whatsapp"
token_env = "WUPHF_CONNECTOR_WHATSAPP_MAIN_TOKEN"

[[connectors]]
id = "telegram-main"
platform = "telegram"
token_env = "WUPHF_CONNECTOR_TELEGRAM_MAIN_TOKEN"
supports_threads = true
supports_topic_create = true
"#;

/// A many-to-one rule from WhatsApp, every chat, with an optional priority.
fn catch_all(id: &str, priority: Option<i32>) -> String {
    let priority = priority.map_or(String::new(), |p| format!("priority = {p}\n"));
    format!(
        "[rules.{id}]\nkind = \"many-to-one\"\nsource_connector = \"whatsapp-main\"\n\
         destination = \"telegram-main:-100{id}\"\n{priority}\n"
    )
}

/// The rule that wins for a WhatsApp chat, loading `rules` with the two
/// connectors.
fn winning_rule(jail: &mut Jail, rules: &str, chat_id: &str) -> String {
    jail.create_file("wuphf.toml", &format!("{MINIMAL}\n{rules}\n{CONNECTORS}"))
        .unwrap();
    let store = store(every_token_set(), Path::new("wuphf.toml")).unwrap();
    let routing = Arc::clone(&store.current().derived.routing);
    let matched = routing
        .match_source(
            &ConnectorId("whatsapp-main".into()),
            &AddressSet::single(chat(chat_id, None)),
        )
        .expect("a rule matches");
    matched.rule.id.0.clone()
}

#[test]
fn rules_are_ordered_by_priority_then_id() {
    Jail::expect_with(|jail| {
        let ties = format!("{}{}", catch_all("b", None), catch_all("a", None));
        assert_eq!(winning_rule(jail, &ties, "x@g.us"), "a");
        let with_z = format!("{ties}{}", catch_all("z", Some(-1)));
        assert_eq!(winning_rule(jail, &with_z, "x@g.us"), "z");
        Ok(())
    });
}

#[test]
fn a_one_to_one_exception_beats_a_catch_all() {
    Jail::expect_with(|jail| {
        let rules = format!(
            "{}[rules.vip]\nkind = \"one-to-one\"\npriority = -1\n\
             source = \"whatsapp-main:vip@s.whatsapp.net\"\n\
             destination = \"telegram-main:-100777\"\n",
            catch_all("all", None)
        );
        assert_eq!(winning_rule(jail, &rules, "vip@s.whatsapp.net"), "vip");
        assert_eq!(winning_rule(jail, &rules, "other@g.us"), "all");
        Ok(())
    });
}

/// The message a config with `rules` (and the two connectors) fails with.
fn rules_error(jail: &mut Jail, rules: &str) -> String {
    jail.create_file("wuphf.toml", &format!("{MINIMAL}\n{rules}\n{CONNECTORS}"))
        .unwrap();
    store_error(every_token_set(), Path::new("wuphf.toml"))
}

#[test]
fn rule_id_field_is_rejected() {
    Jail::expect_with(|jail| {
        let rules = format!("{}id = \"x\"\n", catch_all("x", None));
        let error = rules_error(jail, &rules);
        assert!(
            error.contains("rule \"x\": remove the id field; a rule's id is its key"),
            "{error}"
        );
        Ok(())
    });
}

#[test]
fn rule_id_with_a_dot_is_rejected() {
    Jail::expect_with(|jail| {
        let rules = "[rules.\"a.b\"]\nkind = \"many-to-one\"\n\
                     source_connector = \"whatsapp-main\"\n\
                     destination = \"telegram-main:-100\"\n";
        let error = rules_error(jail, rules);
        assert!(
            error.contains("rule id \"a.b\" must be non-empty and must not contain '.'"),
            "{error}"
        );
        Ok(())
    });
}

#[test]
fn filter_chat_on_another_connector_is_rejected() {
    Jail::expect_with(|jail| {
        let rules = format!(
            "{}except_chats = [\"instagram-main:1\"]\n",
            catch_all("mirror", None)
        );
        let error = rules_error(jail, &rules);
        assert!(
            error.contains(
                "rule \"mirror\": filter chat instagram-main:1 is not on source connector \
                 whatsapp-main"
            ),
            "{error}"
        );
        Ok(())
    });
}

#[test]
fn only_and_except_chats_together_are_rejected() {
    Jail::expect_with(|jail| {
        let rules = format!(
            "{}only_chats = [\"whatsapp-main:a@g.us\"]\nexcept_chats = [\"whatsapp-main:b@g.us\"]\n",
            catch_all("mirror", None)
        );
        let error = rules_error(jail, &rules);
        assert!(
            error.contains("rule \"mirror\": use only_chats or except_chats, not both"),
            "{error}"
        );
        Ok(())
    });
}

#[test]
fn invalid_chat_strings_are_rejected() {
    Jail::expect_with(|jail| {
        let error = extract(jail, &format!("notify = [\"no-colon\"]\n{MINIMAL}"))
            .expect_err("a chat needs its connector");
        assert!(error.to_string().contains("\"no-colon\""), "{error}");
        Ok(())
    });
}

#[test]
fn connector_ids_with_separators_are_rejected() {
    Jail::expect_with(|jail| {
        jail.create_file(
            "wuphf.toml",
            &format!(
                "{MINIMAL}\n[[connectors]]\nid = \"tg:main\"\nplatform = \"telegram\"\n\
                 token_env = \"WUPHF_CONNECTOR_TG_TOKEN\"\n"
            ),
        )?;
        let error = store_error(every_token_set(), Path::new("wuphf.toml"));
        assert!(
            error.contains("connector id \"tg:main\" must not be empty or contain"),
            "{error}"
        );
        Ok(())
    });
}

#[test]
fn derive_rejects_an_unknown_platform() {
    Jail::expect_with(|jail| {
        jail.create_file(
            "wuphf.toml",
            &format!(
                r#"{MINIMAL}
                [[connectors]]
                id = "fax-main"
                platform = "fax"
                token_env = "WUPHF_CONNECTOR_FAX_MAIN_TOKEN"
                "#
            ),
        )?;
        let error = store_error(every_token_set(), Path::new("wuphf.toml"));
        assert!(error.contains("unsupported platform fax"), "{error}");
        Ok(())
    });
}

#[test]
fn derive_rejects_a_provider_without_its_token() {
    Jail::expect_with(|jail| {
        jail.create_file(
            "wuphf.toml",
            &format!(
                r#"{MINIMAL}
                [[functions.providers]]
                id = "mediaconv"
                token_env = "WUPHF_PROVIDER_MEDIACONV_TOKEN"
                functions = ["media.convert"]
                "#
            ),
        )?;
        let error = store_error(WuphfSchema::with_env(|_| None), Path::new("wuphf.toml"));
        assert!(
            error.contains("invalid [functions] configuration"),
            "{error}"
        );
        assert!(error.contains("WUPHF_PROVIDER_MEDIACONV_TOKEN"), "{error}");
        Ok(())
    });
}

#[test]
fn missing_file_is_reported() {
    let error = base(Path::new("/nonexistent/wuphf.toml")).unwrap_err();
    assert!(matches!(error, LoadError::NotFound(_)));
    assert!(
        error.to_string().contains("/nonexistent/wuphf.toml"),
        "{error}"
    );
}

/// The error loading `path` with `schema` fails with: the base configuration
/// is invalid, for the reason in the returned message.
fn store_error(schema: WuphfSchema, path: &Path) -> String {
    match store(schema, path) {
        Ok(_) => panic!("{} should not load", path.display()),
        Err(error @ SettingsError::InvalidBase(_)) => error.to_string(),
        Err(other) => panic!("expected an invalid base configuration, got {other}"),
    }
}

/// A fresh SQLite database in the temp directory, and its URL.
fn temp_database() -> String {
    let path = std::env::temp_dir().join(format!(
        "wuphf-settings-{}.sqlite",
        wuphf_model::DispatchId::new()
    ));
    format!("sqlite://{}", path.display())
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

/// The example configuration's settings store over the SQLite database at
/// `url`, as `serve` loads it.
async fn live_store(url: &str) -> (Arc<SettingsStore>, Arc<SqliteStorage>) {
    let sqlite = Arc::new(SqliteStorage::connect(url).await.unwrap());
    let store = load(
        every_token_set(),
        base(Path::new(EXAMPLE)).unwrap(),
        SettingsStorage::new(Arc::clone(&sqlite)),
    )
    .await
    .unwrap();
    (Arc::new(store), sqlite)
}

/// A one-to-one rule between two of the example's connectors, as the value
/// of the whole `rules` map.
fn runtime_rule(destination_connector: &str) -> serde_json::Value {
    serde_json::json!({ "runtime-direct": runtime_rule_entry(destination_connector) })
}

/// One rule entry: what `rules.<id>` holds.
fn runtime_rule_entry(destination_connector: &str) -> serde_json::Value {
    serde_json::json!({
        "kind": "one-to-one",
        "source": "whatsapp-main:12345@s.whatsapp.net",
        "destination": format!("{destination_connector}:-100123"),
    })
}

fn runtime_rule_id() -> RuleId {
    RuleId("runtime-direct".into())
}

#[test]
fn rules_set_at_runtime_reach_live_routing() {
    Jail::expect_with(|_| {
        runtime().block_on(async {
            let (store, _) = live_store(&temp_database()).await;
            let live = LiveRouting::new(Arc::clone(&store));
            assert!(live.routing().rule(&runtime_rule_id()).is_none());

            store
                .set("rules", runtime_rule("telegram-main"), None)
                .await
                .unwrap();
            assert!(live.routing().rule(&runtime_rule_id()).is_some());
        });
        Ok(())
    });
}

#[test]
fn invalid_rules_are_rejected_and_not_stored() {
    Jail::expect_with(|_| {
        runtime().block_on(async {
            let (store, sqlite) = live_store(&temp_database()).await;
            let live = LiveRouting::new(Arc::clone(&store));
            let before = live.routing();

            let error = store
                .set("rules", runtime_rule("discord-main"), None)
                .await
                .unwrap_err();
            assert!(
                matches!(error, SettingsError::InvalidValue { .. }),
                "{error}"
            );
            assert!(error.to_string().contains("discord-main"), "{error}");
            assert!(Arc::ptr_eq(&before, &live.routing()));
            assert!(sqlite.settings_overrides().await.unwrap().is_empty());
        });
        Ok(())
    });
}

#[test]
fn only_rules_and_notify_change_at_runtime() {
    Jail::expect_with(|_| {
        runtime().block_on(async {
            let (store, _) = live_store(&temp_database()).await;
            for key in ["connectors", "listen", "database_url", "functions"] {
                let error = store
                    .set(key, serde_json::json!("anything"), None)
                    .await
                    .unwrap_err();
                assert!(
                    matches!(error, SettingsError::UnknownKey(_)),
                    "{key}: {error}"
                );
            }
            store
                .set("notify", serde_json::json!(["telegram-main:-100123"]), None)
                .await
                .unwrap();
        });
        Ok(())
    });
}

/// A rules override is stored in SQLite and applied again by a store loaded
/// over the reopened database, as after a restart.
#[test]
fn rules_override_survives_a_restart() {
    Jail::expect_with(|_| {
        let url = temp_database();
        runtime().block_on(async {
            let (store, _) = live_store(&url).await;
            store
                .set("rules", runtime_rule("telegram-main"), None)
                .await
                .unwrap();

            let (restarted, _) = live_store(&url).await;
            let snapshot = restarted.current();
            assert!(snapshot.ignored().is_empty());
            assert!(snapshot.derived.routing.rule(&runtime_rule_id()).is_some());
            assert_eq!(snapshot.source("rules"), Source::Stored);
        });
        Ok(())
    });
}

#[test]
fn unset_rules_falls_back_to_the_file() {
    Jail::expect_with(|_| {
        runtime().block_on(async {
            let (store, sqlite) = live_store(&temp_database()).await;
            store
                .set("rules", runtime_rule("telegram-main"), None)
                .await
                .unwrap();

            store.unset("rules").await.unwrap().unwrap();
            let routing = LiveRouting::new(Arc::clone(&store)).routing();
            assert!(routing.rule(&runtime_rule_id()).is_none());
            assert!(
                routing
                    .rule(&RuleId("wa-to-telegram-mirror".into()))
                    .is_some()
            );
            assert_eq!(store.current().source("rules"), Source::File);
            assert!(sqlite.settings_overrides().await.unwrap().is_empty());
        });
        Ok(())
    });
}

#[test]
fn adapter_stores_the_actor_as_text() {
    runtime().block_on(async {
        let sqlite = Arc::new(SqliteStorage::connect(&temp_database()).await.unwrap());
        let adapter = SettingsStorage::new(Arc::clone(&sqlite));
        adapter
            .write(
                &[],
                Some(&StoredOverride {
                    key: "notify".into(),
                    value: "[]".into(),
                    by: Some("telegram-main:704338780".into()),
                }),
            )
            .await
            .unwrap();
        let stored = sqlite.settings_overrides().await.unwrap();
        assert_eq!(
            stored[0].updated_by.as_deref(),
            Some("telegram-main:704338780")
        );
        let loaded = adapter.load().await.unwrap();
        assert_eq!(loaded[0].by.as_deref(), Some("telegram-main:704338780"));
    });
}

#[test]
fn a_single_rule_can_be_set_and_removed_at_runtime() {
    Jail::expect_with(|_| {
        runtime().block_on(async {
            let (store, _) = live_store(&temp_database()).await;
            let file_rules = [
                RuleId("wa-to-telegram-mirror".into()),
                RuleId("ig-to-telegram-mirror".into()),
            ];
            let extra = RuleId("extra".into());

            store
                .set("rules.extra", runtime_rule_entry("telegram-main"), None)
                .await
                .unwrap();
            let routing = store.current().derived.routing.clone();
            assert!(routing.rule(&extra).is_some());
            for id in &file_rules {
                assert!(routing.rule(id).is_some(), "{} kept", id.0);
            }

            store.delete_entry("rules.extra", None).await.unwrap();
            let routing = store.current().derived.routing.clone();
            assert!(routing.rule(&extra).is_none());
            for id in &file_rules {
                assert!(routing.rule(id).is_some(), "{} kept", id.0);
            }

            store.unset("rules").await.unwrap().unwrap();
            let routing = store.current().derived.routing.clone();
            for id in &file_rules {
                assert!(routing.rule(id).is_some(), "{} back", id.0);
            }
            assert_eq!(store.current().source("rules"), Source::File);
        });
        Ok(())
    });
}
