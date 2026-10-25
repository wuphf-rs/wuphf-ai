//! Loading and deriving the core configuration. Every test that reads the
//! environment runs inside a `figment::Jail`, which serializes them and
//! restores the environment afterwards.
#![allow(clippy::result_large_err)] // `figment::Jail` returns `figment::Error`.

use crate::{Configuration, LoadError, SettingsStorage, SettingsStore, WuphfSchema, base, load};
use botconf::{MemoryStorage, SettingsError, Storage, StoredOverride};
use figment::Jail;
use std::path::Path;
use std::sync::Arc;
use wuphf_model::{ChatFilter, ConnectorId, RuleId, RuleKind};
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
        for rule in &configuration.rules {
            let RuleKind::ManyToOne {
                source_chats_filter,
                destination_chat,
                ..
            } = &rule.flow
            else {
                panic!("example rules are many-to-one mirrors");
            };
            assert_eq!(*source_chats_filter, ChatFilter::All);
            assert_eq!(destination_chat.chat_id, "telegram:channel:2060340733");
        }
        assert_eq!(configuration.notify.len(), 1);
        assert_eq!(
            configuration.notify[0].connector_id,
            ConnectorId("telegram-main".into())
        );
        assert_eq!(
            configuration.notify[0].address.chat_id,
            "telegram:user:704338780"
        );
        assert_eq!(configuration.notify[0].address.thread_id, None);
        Ok(())
    });
}

#[test]
fn one_to_one_rule_and_filters_parse() {
    Jail::expect_with(|jail| {
        let configuration = extract(
            jail,
            &format!(
                r#"{MINIMAL}
                [[rules]]
                id = "direct"
                [rules.flow]
                kind = "one-to-one"
                source_connector = "whatsapp-main"
                source_chat = {{ chat_id = "123@s.whatsapp.net" }}
                destination_connector = "telegram-main"
                destination_chat = {{ chat_id = "-100123", thread_id = "12" }}

                [[rules]]
                id = "filtered-mirror"
                [rules.flow]
                kind = "many-to-one"
                source_connector = "whatsapp-main"
                destination_connector = "telegram-main"
                destination_chat = {{ chat_id = "-100999" }}
                [rules.flow.source_chats_filter]
                kind = "not"
                chats = [{{ chat_id = "spam@g.us" }}]
                "#
            ),
        )?;
        let RuleKind::OneToOne {
            source_chat,
            destination_chat,
            ..
        } = &configuration.rules[0].flow
        else {
            panic!("first rule is one-to-one");
        };
        assert_eq!(source_chat.chat_id, "123@s.whatsapp.net");
        assert_eq!(destination_chat.thread_id.as_deref(), Some("12"));
        let RuleKind::ManyToOne {
            source_chats_filter: ChatFilter::Not { chats },
            ..
        } = &configuration.rules[1].flow
        else {
            panic!("second rule is a filtered mirror");
        };
        assert_eq!(chats[0].chat_id, "spam@g.us");
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

/// An override written through the adapter is read back by a store loaded
/// over a reopened database. No key is a runtime key yet, so the store keeps
/// it as ignored (with a reason) rather than applying it; plan 037 replaces
/// this with a real key.
#[test]
fn sqlite_overrides_survive_a_restart() {
    Jail::expect_with(|_| {
        let url = temp_database();
        runtime().block_on(async {
            let sqlite = Arc::new(SqliteStorage::connect(&url).await.unwrap());
            SettingsStorage::new(sqlite)
                .write(
                    &[],
                    Some(&StoredOverride {
                        key: "rules".into(),
                        value: "[]".into(),
                        by: Some(1),
                    }),
                )
                .await
                .unwrap();

            let reopened = Arc::new(SqliteStorage::connect(&url).await.unwrap());
            let store = load(
                every_token_set(),
                base(Path::new(EXAMPLE)).unwrap(),
                SettingsStorage::new(reopened),
            )
            .await
            .unwrap();
            assert!(store.current().ignored().contains_key("rules"));
        });
        Ok(())
    });
}

#[test]
fn adapter_stores_the_actor_as_text() {
    runtime().block_on(async {
        let sqlite = Arc::new(SqliteStorage::connect(&temp_database()).await.unwrap());
        SettingsStorage::new(Arc::clone(&sqlite))
            .write(
                &[],
                Some(&StoredOverride {
                    key: "notify".into(),
                    value: "[]".into(),
                    by: Some(42),
                }),
            )
            .await
            .unwrap();
        let stored = sqlite.settings_overrides().await.unwrap();
        assert_eq!(stored[0].updated_by.as_deref(), Some("42"));
    });
}
