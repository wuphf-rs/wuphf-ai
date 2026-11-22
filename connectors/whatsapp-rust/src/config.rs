use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use wuphf_connector::logging::{LEVELS, check_level};
use wuphf_connector::settings::{RuntimeSettings, Setting, SettingKind};

/// Connector-local configuration. Routing is owned entirely by the core; this
/// file only carries how to reach the core and how to run the WhatsApp
/// session.
#[derive(Debug, Deserialize)]
pub struct Config {
    /// The core's gRPC address.
    pub core_address: String,
    /// Must match a `[[connectors]]` entry in the core's configuration.
    pub connector_id: String,
    /// Environment variable holding this connector's core token; must match the
    /// core's `[[connectors]].token_env` value for this connector id. Required.
    pub core_token_env: String,
    /// Path of the SQLite database holding the WhatsApp device session.
    /// Reopening the same path restores the paired session.
    pub session_database: String,
    /// The account's phone number (country code, digits only), which a pair
    /// code is issued for. Required by `login_method = "pair_code"`.
    #[serde(default)]
    pub phone_number: Option<String>,
    /// How the session is linked, at first start and again after a logout:
    /// `"qr"` (scan a QR code) or `"pair_code"` (type an 8-character code into
    /// WhatsApp). Unset: `pair_code` when `phone_number` is set, else `qr`.
    #[serde(default)]
    login_method: Option<LoginMethod>,
    /// Stamp each chat's disappearing-messages timer on what this connector
    /// sends, look a group's timer up when the core does not know it, and
    /// warn the owner when a DM's timer is unknown. Part of the registration,
    /// so it must match `supports_disappearing_timers` in the core's
    /// `[[connectors]]` entry for this connector, or the core refuses it.
    #[serde(default)]
    pub supports_disappearing_timers: bool,
    /// Which WhatsApp client the session registers as. Both pair as an Android
    /// client, so WhatsApp delivers view-once media, which it withholds from a
    /// web companion; they differ only in the account type they match.
    /// Applied at pairing, so a change needs a fresh pairing.
    #[serde(default)]
    pub client_mode: ClientMode,
    /// Environment variable holding the core's invoke token, when the core
    /// sets `[functions] invoke_token_env`; unset or empty means none.
    #[serde(default)]
    pub functions_token_env: Option<String>,
    /// Settings that may change while the connector runs: declared to the core,
    /// which pushes every change back (see [`RuntimeSettings`]). Flattened, so
    /// the file keeps its layout (`log_level`, `[rendering]`).
    #[serde(flatten)]
    pub runtime: Runtime,
}

/// Settings that may change while the connector runs, the shape of the Go
/// connector's `config.Runtime`.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct Runtime {
    /// Log verbosity (trace, debug, info, warn, error; empty means info); the
    /// WUPHF_LOG_LEVEL environment variable overrides it.
    #[serde(default)]
    pub log_level: String,
    /// Bounds a sticker conversion, waiting for the converter included;
    /// deliveries on this connector wait behind it. 0 means 20000.
    #[serde(default)]
    pub media_convert_timeout_ms: u32,
    /// The pack metadata embedded in stickers bridged to WhatsApp; blank
    /// fields fall back to the defaults (see [`Runtime::sticker_metadata`]).
    #[serde(default)]
    pub sticker_metadata: StickerMetadata,
    #[serde(default)]
    pub rendering: Rendering,
}

const DEFAULT_MEDIA_CONVERT_TIMEOUT: Duration = Duration::from_secs(20);

/// Default sticker-pack metadata, for any field the configuration leaves
/// blank, so bridged stickers are always attributed.
const DEFAULT_STICKER_PACK_NAME: &str = "wuphf";
const DEFAULT_STICKER_AUTHOR: &str = "wuphf-ai";
const DEFAULT_STICKER_LINK: &str = "https://github.com/wuphf/wuphf-ai";

impl Runtime {
    /// The sticker conversion bound (Go `MediaConvertTimeout`).
    pub fn media_convert_timeout(&self) -> Duration {
        match self.media_convert_timeout_ms {
            0 => DEFAULT_MEDIA_CONVERT_TIMEOUT,
            ms => Duration::from_millis(u64::from(ms)),
        }
    }

    /// The configured sticker-pack metadata with the defaults filled in for
    /// any blank field (Go `StickerMetadata`).
    pub fn sticker_metadata(&self) -> StickerMetadata {
        let or = |value: &str, default: &str| match value {
            "" => default.to_owned(),
            value => value.to_owned(),
        };
        let meta = &self.sticker_metadata;
        StickerMetadata {
            pack_name: or(&meta.pack_name, DEFAULT_STICKER_PACK_NAME),
            author_name: or(&meta.author_name, DEFAULT_STICKER_AUTHOR),
            link: or(&meta.link, DEFAULT_STICKER_LINK),
        }
    }

    /// What an empty setting stands for, filled in, so the values declared to
    /// the core are explicit (Go `WithDefaults`).
    pub fn with_defaults(mut self) -> Self {
        self.log_level = match self.log_level.to_lowercase() {
            level if level.is_empty() => "info".into(),
            level => level,
        };
        self.sticker_metadata = self.sticker_metadata();
        self
    }
}

/// The Go connector's runtime settings, word for word and in its order
/// (`connectors/whatsapp/internal/config/config.go`), so the settings panel
/// and `wuphf settings` treat both connectors alike; the shared fixture test
/// checks it.
impl RuntimeSettings for Runtime {
    fn settings() -> Vec<Setting> {
        let up_to_u32 = SettingKind::Number {
            min: 0,
            max: i64::from(u32::MAX),
        };
        vec![
            Setting {
                key: "log_level",
                title: "Log level",
                description: "How much the connector logs (WUPHF_LOG_LEVEL wins)",
                kind: SettingKind::Choice(&LEVELS),
            },
            Setting {
                key: "media_convert_timeout_ms",
                title: "Sticker conversion timeout (ms)",
                description: "How long converting a sticker may take, waiting for the converter \
                              included; 0 means 20000",
                kind: up_to_u32.clone(),
            },
            Setting {
                key: "sticker_metadata.pack_name",
                title: "Sticker pack name",
                description: "The pack name WhatsApp shows for bridged stickers",
                kind: SettingKind::Text,
            },
            Setting {
                key: "sticker_metadata.author_name",
                title: "Sticker author",
                description: "The author WhatsApp shows for bridged stickers",
                kind: SettingKind::Text,
            },
            Setting {
                key: "sticker_metadata.link",
                title: "Sticker link",
                description: "A link folded into the sticker's publisher",
                kind: SettingKind::Text,
            },
            Setting {
                key: "rendering.attachments_as_files",
                title: "Attachments as files",
                description: "Send every attachment as a file, whatever its kind",
                kind: SettingKind::Bool,
            },
            Setting {
                key: "rendering.max_message_length",
                title: "Longest inline message",
                description: "Longer text is sent as a file; 0 means no limit",
                kind: up_to_u32,
            },
        ]
    }
}

/// How mirrored copies are rendered on WhatsApp: this connector's own policy;
/// the core dispatches content only.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct Rendering {
    /// Send every attachment as a file, whatever its kind.
    #[serde(default)]
    pub attachments_as_files: bool,
    /// Longer text (in characters) is sent as a plain-text file; 0 means no
    /// limit.
    #[serde(default)]
    pub max_message_length: u32,
}

/// The sticker-pack metadata embedded in stickers bridged to WhatsApp, shown in
/// WhatsApp's sticker info. A blank field falls back to its default.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct StickerMetadata {
    #[serde(default)]
    pub pack_name: String,
    #[serde(default)]
    pub author_name: String,
    #[serde(default)]
    pub link: String,
}

/// The WhatsApp client kind the session registers as.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ClientMode {
    /// A personal WhatsApp account.
    #[default]
    Android,
    /// A WhatsApp Business (SMB) account, which cannot pair as the personal
    /// client.
    AndroidBusiness,
}

/// How the WhatsApp session is linked to the account.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LoginMethod {
    /// Scan a QR code in WhatsApp → Linked devices → Link a device.
    Qr,
    /// Type an 8-character code into WhatsApp after "Link with phone number
    /// instead"; needs `phone_number`.
    PairCode,
}

impl Config {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let contents = std::fs::read_to_string(path)
            .map_err(|error| anyhow::anyhow!("read config {}: {error}", path.display()))?;
        Self::parse(&contents)
            .map_err(|error| anyhow::anyhow!("parse config {}: {error}", path.display()))
    }

    fn parse(contents: &str) -> anyhow::Result<Self> {
        let config: Self = toml::from_str(contents)?;
        check_level(&config.runtime.log_level).map_err(anyhow::Error::msg)?;
        if config.login_method() == LoginMethod::PairCode && config.phone_number.is_none() {
            anyhow::bail!("login_method = \"pair_code\" needs phone_number");
        }
        Ok(config)
    }

    /// The configured login method, defaulted from `phone_number`: before the
    /// key existed, a set phone number meant pairing by code.
    pub fn login_method(&self) -> LoginMethod {
        self.login_method.unwrap_or(match self.phone_number {
            Some(_) => LoginMethod::PairCode,
            None => LoginMethod::Qr,
        })
    }

    /// The phone number a pair code is issued for, when the session pairs by
    /// code.
    pub fn pair_code_phone(&self) -> Option<&str> {
        match self.login_method() {
            LoginMethod::PairCode => self.phone_number.as_deref(),
            LoginMethod::Qr => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = r#"
        core_address = "127.0.0.1:50051"
        connector_id = "whatsapp-rust"
        core_token_env = "TOKEN"
        session_database = "session.db"
    "#;

    #[test]
    fn client_mode_defaults_to_android() {
        let config = Config::parse(MINIMAL).unwrap();
        assert_eq!(config.client_mode, ClientMode::Android);
    }

    #[test]
    fn client_mode_reads_business() {
        let config =
            Config::parse(&format!("{MINIMAL}client_mode = \"android_business\"\n")).unwrap();
        assert_eq!(config.client_mode, ClientMode::AndroidBusiness);
    }

    #[test]
    fn the_login_method_defaults_from_the_phone_number() {
        let config = Config::parse(MINIMAL).unwrap();
        assert_eq!(config.login_method(), LoginMethod::Qr);
        assert_eq!(config.pair_code_phone(), None);
        let config = Config::parse(&format!("{MINIMAL}phone_number = \"15551234567\"\n")).unwrap();
        assert_eq!(config.login_method(), LoginMethod::PairCode);
        assert_eq!(config.pair_code_phone(), Some("15551234567"));
    }

    #[test]
    fn qr_login_ignores_a_phone_number() {
        let config = Config::parse(&format!(
            "{MINIMAL}phone_number = \"15551234567\"\nlogin_method = \"qr\"\n"
        ))
        .unwrap();
        assert_eq!(config.login_method(), LoginMethod::Qr);
        assert_eq!(config.pair_code_phone(), None);
    }

    #[test]
    fn pair_code_login_needs_a_phone_number() {
        let error = Config::parse(&format!("{MINIMAL}login_method = \"pair_code\"\n")).unwrap_err();
        assert!(error.to_string().contains("needs phone_number"), "{error}");
    }

    #[test]
    fn rendering_defaults_off_and_reads_its_section() {
        let config = Config::parse(MINIMAL).unwrap();
        assert_eq!(config.runtime, Runtime::default());
        let config = Config::parse(&format!(
            "{MINIMAL}[rendering]\nattachments_as_files = true\nmax_message_length = 4096\n"
        ))
        .unwrap();
        assert!(config.runtime.rendering.attachments_as_files);
        assert_eq!(config.runtime.rendering.max_message_length, 4096);
    }

    #[test]
    fn sticker_settings_default_like_go() {
        let config = Config::parse(MINIMAL).unwrap();
        assert_eq!(
            config.runtime.sticker_metadata(),
            StickerMetadata {
                pack_name: "wuphf".into(),
                author_name: "wuphf-ai".into(),
                link: "https://github.com/wuphf/wuphf-ai".into(),
            }
        );
        assert_eq!(
            config.runtime.media_convert_timeout(),
            Duration::from_secs(20)
        );
        assert_eq!(config.functions_token_env, None);

        let config = Config::parse(&format!(
            "functions_token_env = \"INVOKE\"\nmedia_convert_timeout_ms = 5000\n{MINIMAL}\
             [sticker_metadata]\npack_name = \"mine\"\n"
        ))
        .unwrap();
        assert_eq!(config.functions_token_env.as_deref(), Some("INVOKE"));
        assert_eq!(
            config.runtime.media_convert_timeout(),
            Duration::from_secs(5)
        );
        let meta = config.runtime.sticker_metadata();
        assert_eq!(
            (meta.pack_name.as_str(), meta.author_name.as_str()),
            ("mine", "wuphf-ai")
        );
    }

    /// The declaration the Go connector writes, checked by its own test
    /// (`TestRuntimeDeclarationMatchesTheSharedFixture`).
    const GO_DECLARATION: &str =
        include_str!("../../whatsapp/internal/config/testdata/runtime-declaration.json");

    #[test]
    fn the_declaration_matches_the_go_connector() {
        use prost::Message as _;
        use wuphf_connector::settings::proto::DescribeResponse;

        let go: serde_json::Value = serde_json::from_str(GO_DECLARATION).unwrap();
        let ours = wuphf_connector::settings::declare(&Runtime::default().with_defaults()).unwrap();

        let hex = go["descriptor_hex"].as_str().unwrap();
        let go_descriptor: Vec<u8> = (0..hex.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(&hex[at..at + 2], 16).unwrap())
            .collect();
        assert_eq!(
            DescribeResponse::decode(ours.descriptor.as_slice()).unwrap(),
            DescribeResponse::decode(go_descriptor.as_slice()).unwrap()
        );

        // Go's reflector names its package in `$id`; nothing reads it.
        let mut go_schema = go["json_schema"].clone();
        go_schema.as_object_mut().unwrap().remove("$id");
        let our_schema: serde_json::Value = serde_json::from_str(&ours.json_schema).unwrap();
        assert_eq!(our_schema, go_schema);

        let our_base: serde_json::Value = serde_json::from_str(&ours.base_json).unwrap();
        assert_eq!(our_base, go["base"]);
    }

    #[test]
    fn the_log_level_is_checked() {
        let error = Config::parse(&format!("log_level = \"VERBOSE\"\n{MINIMAL}")).unwrap_err();
        assert!(
            error.to_string().contains("log_level must be one of"),
            "{error}"
        );
        let config = Config::parse(&format!("log_level = \"INFO\"\n{MINIMAL}")).unwrap();
        assert_eq!(config.runtime.with_defaults().log_level, "info");
        let config = Config::parse(MINIMAL).unwrap();
        assert_eq!(config.runtime.log_level, "");
        assert_eq!(config.runtime.with_defaults().log_level, "info");
    }

    #[test]
    fn the_example_parses() {
        let config = Config::parse(include_str!("../config.example.toml")).unwrap();
        assert_eq!(config.runtime.sticker_metadata().pack_name, "wuphf");
    }

    #[test]
    fn an_unknown_client_mode_is_refused() {
        let error = Config::parse(&format!("{MINIMAL}client_mode = \"web\"\n")).unwrap_err();
        assert!(error.to_string().contains("unknown variant"), "{error}");
    }
}
