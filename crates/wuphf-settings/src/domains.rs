//! The reference domains of the core's settings: what the settings panel
//! needs to know to show and ask for the chats, connectors and rules a
//! setting holds. Their names come from the core's storage (see
//! `wuphf_runtime::settings_auth::WuphfNames`).

use botconf_panel::Domain;
use serde_json::Value;
use std::str::FromStr;
use wuphf_model::{ChatRef, is_valid_connector_id};

/// A chat on a connector, written `connector:chat` or `connector:chat#thread`.
pub const CHAT: &str = "wuphf.chat";
/// A configured connector id.
pub const CONNECTOR: &str = "wuphf.connector";
/// A rule id.
pub const RULE: &str = "wuphf.rule";

/// Every domain, to register on the settings panel.
pub fn all() -> [Domain; 3] {
    [
        Domain {
            noun: ("chat", "chats"),
            one: "the chat, as connector:chat",
            many: "the chats, as connector:chat",
            example: Some("telegram-main:telegram:channel:2060340733"),
            unknown: unknown_chat,
            ..Domain::new(CHAT, parse_chat)
        },
        Domain {
            noun: ("connector", "connectors"),
            one: "the connector id",
            many: "the connector ids",
            example: Some("telegram-main"),
            ..Domain::new(CONNECTOR, parse_connector)
        },
        Domain {
            noun: ("rule", "rules"),
            one: "the rule id",
            many: "the rule ids",
            ..Domain::new(RULE, parse_rule)
        },
    ]
}

/// A chat reference in its canonical text form.
fn parse_chat(text: &str) -> Option<Value> {
    let chat = ChatRef::from_str(text.trim()).ok()?;
    Some(Value::String(chat.to_string()))
}

fn unknown_chat(word: &str) -> String {
    format!("`{word}` is not a chat: write connector:chat, e.g. telegram-main:-1001234567890")
}

fn parse_connector(text: &str) -> Option<Value> {
    let id = text.trim();
    is_valid_connector_id(id).then(|| Value::String(id.to_owned()))
}

fn parse_rule(text: &str) -> Option<Value> {
    let id = text.trim();
    botconf::keys::is_entry_name(id).then(|| Value::String(id.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chats_parse_to_their_canonical_form() {
        let [chat, connector, rule] = all();
        assert_eq!(
            (chat.parse)(" whatsapp-main:a%23b#7 "),
            Some(Value::String("whatsapp-main:a%23b#7".into()))
        );
        assert_eq!((chat.parse)("no-chat"), None);
        assert_eq!(
            (connector.parse)("telegram-main"),
            Some(Value::String("telegram-main".into()))
        );
        assert_eq!((connector.parse)("tele:gram"), None);
        assert_eq!((rule.parse)("a.b"), None);
    }

    /// The config types name domains as literals (`#[botconf(domain =
    /// "…")]`): each must be one the panel registers, and the chat and
    /// connector options must name theirs.
    #[test]
    fn the_config_names_registered_domains() {
        use botconf::describe::{Node, Table};

        fn walk(table: &Table, path: &str, out: &mut Vec<(String, String)>) {
            for entry in &table.entries {
                let path = format!("{path}{}", entry.key);
                if let Some(domain) = &entry.domain {
                    out.push((path.clone(), domain.clone()));
                }
                let mut node = &entry.node;
                while let Node::List(inner) | Node::Map(inner) = node {
                    node = inner;
                }
                match node {
                    Node::Table(table) => walk(table, &format!("{path}."), out),
                    Node::Tagged {
                        common, variants, ..
                    } => {
                        walk(common, &format!("{path}."), out);
                        for variant in variants {
                            walk(&variant.table, &format!("{path}.{}.", variant.value), out);
                        }
                    }
                    _ => {}
                }
            }
        }
        let mut named = Vec::new();
        walk(&crate::description().table, "", &mut named);
        let registered = all().map(|domain| domain.name);
        for (path, domain) in &named {
            assert!(
                registered.contains(&domain.as_str()),
                "{path}: {domain} is not registered"
            );
        }
        let paths: Vec<&str> = named.iter().map(|(path, _)| path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "rules.one-to-one.source",
                "rules.one-to-one.destination",
                "rules.many-to-one.source_connector",
                "rules.many-to-one.destination",
                "rules.many-to-one.only_chats",
                "rules.many-to-one.except_chats",
                "notify",
                "chat_state.chats.chat",
            ]
        );
    }
}
