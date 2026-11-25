//! The chat directory: the chats a connector's account can reach, fetched by
//! the connector's setup when its account is linked, so the setups of the
//! core and misc can offer chats by name with nothing running.
//!
//! One file per connector, `<connector_id>.chats.json`, in
//! `$WUPHF_CHAT_DIRECTORY`, else `$XDG_STATE_HOME/wuphf/chats`, else
//! `~/.local/state/wuphf/chats`. A connector's setup writes it whole; the
//! Go connectors share the format (`connectors/shared/chatdir`).

use std::{
    fmt, fs, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::{ChatRef, ConnectorId};

/// The format's version; a file of another version is not read.
pub const CHAT_DIRECTORY_VERSION: u32 = 1;

/// The variable naming the directory, over the XDG default.
pub const CHAT_DIRECTORY_ENV: &str = "WUPHF_CHAT_DIRECTORY";

/// One connector's chats.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatDirectory {
    pub version: u32,
    pub connector_id: ConnectorId,
    /// The connector's platform (`whatsapp`, `telegram`, `instagram`).
    pub platform: String,
    /// When the chats were fetched, in Unix milliseconds.
    pub fetched_at_ms: u64,
    pub chats: Vec<DirectoryChat>,
}

/// A chat, as the connector reports it to the core.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectoryChat {
    /// `connector:chat`, the id rules match.
    #[serde(with = "chat_text")]
    pub chat: ChatRef,
    pub title: String,
    pub kind: DirectoryChatKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DirectoryChatKind {
    Group,
    Direct,
    Channel,
    /// A chat grouping others (a WhatsApp community, plan 085): naming it in
    /// a rule's chats covers its members.
    Community,
}

impl fmt::Display for DirectoryChatKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Group => "group",
            Self::Direct => "direct",
            Self::Channel => "channel",
            Self::Community => "community",
        })
    }
}

impl DirectoryChat {
    /// Whether the chat matches what a picker's user typed: a part of its
    /// title (any case) or of its id. Empty text matches every chat.
    pub fn matches(&self, text: &str) -> bool {
        let text = text.trim().to_lowercase();
        self.title.to_lowercase().contains(&text)
            || self.chat.to_string().to_lowercase().contains(&text)
    }
}

/// Why a directory file could not be read or written.
#[derive(Debug, thiserror::Error)]
pub enum ChatDirectoryError {
    #[error("{path}: {source}")]
    Io { path: PathBuf, source: io::Error },
    #[error("{path} is not a chat directory: {message}")]
    Invalid { path: PathBuf, message: String },
    #[error("{path} is version {found}, not {CHAT_DIRECTORY_VERSION}: link the connector again")]
    Version { path: PathBuf, found: u32 },
    #[error("{path} holds the chats of {found}")]
    Connector { path: PathBuf, found: ConnectorId },
}

/// The directory the files are in: `$WUPHF_CHAT_DIRECTORY`, else
/// `$XDG_STATE_HOME/wuphf/chats`, else `~/.local/state/wuphf/chats`; `None`
/// with none of those (or `$HOME`) set.
pub fn chat_directory() -> Option<PathBuf> {
    chat_directory_from(|name| std::env::var_os(name).map(PathBuf::from))
}

/// As [`chat_directory`], with the environment given.
pub fn chat_directory_from(var: impl Fn(&str) -> Option<PathBuf>) -> Option<PathBuf> {
    let set = |name: &str| var(name).filter(|path| !path.as_os_str().is_empty());
    set(CHAT_DIRECTORY_ENV)
        .or_else(|| set("XDG_STATE_HOME").map(|state| state.join("wuphf/chats")))
        .or_else(|| set("HOME").map(|home| home.join(".local/state/wuphf/chats")))
}

impl ChatDirectory {
    pub fn new(
        connector_id: ConnectorId,
        platform: impl Into<String>,
        fetched_at_ms: u64,
        chats: Vec<DirectoryChat>,
    ) -> Self {
        Self {
            version: CHAT_DIRECTORY_VERSION,
            connector_id,
            platform: platform.into(),
            fetched_at_ms,
            chats,
        }
    }

    /// The file of `connector_id`'s chats in `directory`.
    pub fn path(directory: &Path, connector_id: &ConnectorId) -> PathBuf {
        directory.join(format!("{}.chats.json", connector_id.0))
    }

    /// `connector_id`'s chats, or `None` when it has no file yet.
    pub fn read(
        directory: &Path,
        connector_id: &ConnectorId,
    ) -> Result<Option<Self>, ChatDirectoryError> {
        let path = Self::path(directory, connector_id);
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(ChatDirectoryError::Io { path, source }),
        };
        // The version first, so a newer file says so rather than failing on
        // a field it changed.
        #[derive(Deserialize)]
        struct Versioned {
            version: u32,
        }
        let invalid = |error: serde_json::Error| ChatDirectoryError::Invalid {
            path: path.clone(),
            message: error.to_string(),
        };
        let Versioned { version } = serde_json::from_str(&text).map_err(invalid)?;
        if version != CHAT_DIRECTORY_VERSION {
            return Err(ChatDirectoryError::Version {
                path,
                found: version,
            });
        }
        let read: Self = serde_json::from_str(&text).map_err(invalid)?;
        if &read.connector_id != connector_id {
            return Err(ChatDirectoryError::Connector {
                path,
                found: read.connector_id,
            });
        }
        Ok(Some(read))
    }

    /// Writes the file whole (a temporary file, renamed over it), creating
    /// the directory.
    pub fn write(&self, directory: &Path) -> Result<PathBuf, ChatDirectoryError> {
        let path = Self::path(directory, &self.connector_id);
        let io = |source| ChatDirectoryError::Io {
            path: path.clone(),
            source,
        };
        fs::create_dir_all(directory).map_err(io)?;
        let text = serde_json::to_string_pretty(self).expect("a chat directory serializes");
        let temporary = path.with_extension("json.tmp");
        fs::write(&temporary, text + "\n").map_err(io)?;
        fs::rename(&temporary, &path).map_err(io)?;
        Ok(path)
    }
}

/// A chat in its text form, `connector:chat`.
mod chat_text {
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    use crate::ChatRef;

    pub fn serialize<S: Serializer>(chat: &ChatRef, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(chat)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<ChatRef, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(D::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fixture the Go side (`connectors/shared/chatdir`) decodes too.
    const FIXTURE: &str =
        include_str!("../../../connectors/shared/chatdir/testdata/directory.json");

    fn whatsapp() -> ConnectorId {
        ConnectorId("whatsapp-main".into())
    }

    fn temporary() -> PathBuf {
        let path = std::env::temp_dir().join(format!("wuphf-chats-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn the_shared_fixture_reads_and_writes_back() {
        let directory: ChatDirectory = serde_json::from_str(FIXTURE).unwrap();
        assert_eq!(directory.connector_id, whatsapp());
        assert_eq!(directory.chats.len(), 4);
        assert_eq!(directory.chats[1].kind, DirectoryChatKind::Direct);
        let thread = &directory.chats[3].chat;
        assert_eq!(thread.address.chat_id, "odd#id");
        assert_eq!(thread.address.thread_id.as_deref(), Some("7"));
        let written: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&directory).unwrap()).unwrap();
        assert_eq!(
            written,
            serde_json::from_str::<serde_json::Value>(FIXTURE).unwrap()
        );
    }

    #[test]
    fn a_file_is_written_whole_and_read_back() {
        let dir = temporary().join("nested");
        assert!(ChatDirectory::read(&dir, &whatsapp()).unwrap().is_none());
        let directory: ChatDirectory = serde_json::from_str(FIXTURE).unwrap();
        let path = directory.write(&dir).unwrap();
        assert_eq!(path, dir.join("whatsapp-main.chats.json"));
        assert_eq!(
            ChatDirectory::read(&dir, &whatsapp()).unwrap(),
            Some(directory)
        );
        assert!(!path.with_extension("json.tmp").exists());
    }

    #[test]
    fn another_version_or_connector_is_refused() {
        let dir = temporary();
        let path = ChatDirectory::path(&dir, &whatsapp());
        fs::write(&path, r#"{"version": 2, "whatever": true}"#).unwrap();
        assert!(matches!(
            ChatDirectory::read(&dir, &whatsapp()),
            Err(ChatDirectoryError::Version { found: 2, .. })
        ));
        fs::write(&path, FIXTURE.replace("\"whatsapp-main\"", "\"other\"")).unwrap();
        assert!(matches!(
            ChatDirectory::read(&dir, &whatsapp()),
            Err(ChatDirectoryError::Connector { .. })
        ));
        fs::write(&path, "{").unwrap();
        assert!(matches!(
            ChatDirectory::read(&dir, &whatsapp()),
            Err(ChatDirectoryError::Invalid { .. })
        ));
    }

    #[test]
    fn chats_match_by_title_or_id() {
        let directory: ChatDirectory = serde_json::from_str(FIXTURE).unwrap();
        let found = |text: &str| -> Vec<&str> {
            directory
                .chats
                .iter()
                .filter(|chat| chat.matches(text))
                .map(|chat| chat.title.as_str())
                .collect()
        };
        assert_eq!(found("FOOT"), ["Football club news"]);
        assert_eq!(found("9198"), ["Alice"]);
        assert_eq!(found("").len(), 4);
    }

    #[test]
    fn the_directory_comes_from_the_environment() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| PathBuf::from(value))
            }
        };
        assert_eq!(
            chat_directory_from(env(&[
                ("WUPHF_CHAT_DIRECTORY", "/chats"),
                ("XDG_STATE_HOME", "/state")
            ])),
            Some("/chats".into())
        );
        assert_eq!(
            chat_directory_from(env(&[
                ("WUPHF_CHAT_DIRECTORY", ""),
                ("XDG_STATE_HOME", "/state"),
                ("HOME", "/home/u")
            ])),
            Some("/state/wuphf/chats".into())
        );
        assert_eq!(
            chat_directory_from(env(&[("HOME", "/home/u")])),
            Some("/home/u/.local/state/wuphf/chats".into())
        );
        assert_eq!(chat_directory_from(env(&[])), None);
    }
}
