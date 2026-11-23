use super::*;
use botconf::{Storage as _, StoredOverride};

const TOKENS: &str = "WUPHF_CONNECTOR_WHATSAPP_MAIN_TOKEN=w\n\
     WUPHF_CONNECTOR_INSTAGRAM_MAIN_TOKEN=i\n\
     WUPHF_CONNECTOR_TELEGRAM_MAIN_TOKEN=t\n\
     WUPHF_PROVIDER_MEDIACONV_TOKEN=m\n";
const CONNECTOR_KEY: &str = "connector_settings.telegram-main.log_level";
const NOTIFY: &str = r#"["telegram-main:telegram:user:1"]"#;

/// A fresh directory with the example config (its database moved inside)
/// and an env file holding its tokens.
fn workspace() -> (PathBuf, PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!("wuphf-backup-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let example = include_str!("../../../../config/example.toml");
    let toml = example.replace(
        "database_url = \"sqlite://wuphf.sqlite\"",
        &format!(
            "database_url = \"sqlite://{}\"",
            dir.join("core.sqlite").display()
        ),
    );
    assert_ne!(toml, example, "the example config names its database");
    let (config, env_file) = (dir.join("config.toml"), dir.join(".env"));
    std::fs::write(&config, toml).unwrap();
    std::fs::write(&env_file, TOKENS).unwrap();
    (dir, config, env_file)
}

/// The core's own override (notify) and a connector's.
async fn store_overrides(config: &Path, env_file: &Path) {
    let schema = WuphfSchema::with_env(env_lookup(env_file).unwrap());
    let (storage, store) = crate::settings::open(config, schema).await.unwrap();
    store
        .set("notify", serde_json::from_str(NOTIFY).unwrap(), None)
        .await
        .unwrap();
    SettingsStorage::new(storage)
        .write(
            &[],
            Some(&StoredOverride {
                key: CONNECTOR_KEY.into(),
                value: "\"debug\"".into(),
                by: None,
            }),
        )
        .await
        .unwrap();
}

async fn stored(config: &Path, env_file: &Path) -> Vec<String> {
    let schema = WuphfSchema::with_env(env_lookup(env_file).unwrap());
    let (storage, _store) = crate::settings::open(config, schema).await.unwrap();
    let mut keys: Vec<_> = SettingsStorage::new(storage)
        .load()
        .await
        .unwrap()
        .into_iter()
        .map(|row| row.key)
        .collect();
    keys.sort();
    keys
}

#[tokio::test]
async fn a_backup_restores_the_file_and_every_override() {
    let (dir, config, env_file) = workspace();
    store_overrides(&config, &env_file).await;
    let text = export_offline(&config, &env_file, false).await.unwrap();
    assert!(
        text.contains("telegram-main:telegram:user:1"),
        "the override is in the file:\n{text}"
    );
    assert!(
        text.contains(&format!("\"{CONNECTOR_KEY}\" = \"debug\"")),
        "{text}"
    );
    assert!(
        !text.contains("[backup.env_file]"),
        "no secrets without the flag"
    );

    // The file edited by hand since; the backup brings it back.
    std::fs::write(
        &config,
        std::fs::read_to_string(&config)
            .unwrap()
            .replace("50051", "50052"),
    )
    .unwrap();
    let imported = import_offline(&config, &env_file, &text).await.unwrap();
    assert_eq!((imported.removed, imported.stored), (2, 1));
    assert!(imported.kept.contains(&dir.join("config.toml.bak")));
    let restored = std::fs::read_to_string(&config).unwrap();
    assert!(restored.contains("50051") && restored.contains("telegram-main:telegram:user:1"));
    assert_eq!(
        stored(&config, &env_file).await,
        [CONNECTOR_KEY],
        "the core's own is in the file now"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn secrets_come_only_with_the_flag() {
    let (dir, config, env_file) = workspace();
    let text = export_offline(&config, &env_file, true).await.unwrap();
    let backup = Backup::parse(&text).unwrap();
    assert_eq!(
        backup
            .env
            .get("WUPHF_CONNECTOR_TELEGRAM_MAIN_TOKEN")
            .map(String::as_str),
        Some("t")
    );
    assert!(backup.secrets);
    // Restored into an empty env file, the tokens come back.
    std::fs::write(&env_file, "").unwrap();
    let imported = import_offline(&config, &env_file, &text).await.unwrap();
    assert_eq!(imported.env_vars, 4);
    assert!(
        std::fs::read_to_string(&env_file)
            .unwrap()
            .contains("WUPHF_PROVIDER_MEDIACONV_TOKEN=m")
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[tokio::test]
async fn an_import_is_refused_while_a_core_runs_or_the_backup_does_not_load() {
    let (dir, config, env_file) = workspace();
    let text = export_offline(&config, &env_file, false).await.unwrap();
    let before = std::fs::read_to_string(&config).unwrap();

    let held = lock_database(&database_url(&config).unwrap()).unwrap();
    let refused = import_offline(&config, &env_file, &text).await.unwrap_err();
    assert!(
        refused.to_string().contains("a core is running"),
        "{refused}"
    );
    drop(held);

    // Without the tokens the restored file would not start the core.
    std::fs::write(&env_file, "").unwrap();
    let refused = import_offline(&config, &env_file, &text).await.unwrap_err();
    assert!(refused.to_string().contains("does not load"), "{refused:#}");
    assert_eq!(
        std::fs::read_to_string(&config).unwrap(),
        before,
        "nothing written"
    );
    assert!(!dir.join("config.toml.bak").exists());
    std::fs::remove_dir_all(&dir).unwrap();
}
