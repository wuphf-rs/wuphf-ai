//! `wuphf-server setup`: the core's config file and the env file its options
//! name, edited on a setup screen without the core running. Chats are
//! suggested by name from the chat directory (the chats each connector's own
//! setup fetched when its account was linked).

use anyhow::Result;
use botconf_tui::{Query, Suggestion};
use serde_json::Value;
use std::path::{Path, PathBuf};
use wuphf_model::{ChatDirectory, ConnectorId, chat_directory};
use wuphf_settings::domains;

pub fn run(config: PathBuf, env_file: PathBuf) -> Result<()> {
    let directory = chat_directory();
    let setup = botconf_tui::Setup::new(
        wuphf_settings::description(),
        config,
        env_file,
        // The core's own checks, the tokens read from the env file being
        // written before the process environment.
        Box::new(|candidate| {
            let env = candidate.env.clone();
            wuphf_settings::check(candidate.config, move |name| {
                env.get(name).cloned().or_else(|| std::env::var(name).ok())
            })
        }),
    )
    .suggest(move |query| suggest(directory.as_deref(), query));
    botconf_tui::run(setup)?;
    Ok(())
}

/// The connectors the config being edited lists: (id, platform).
fn connectors(values: &Value) -> Vec<(String, String)> {
    values["connectors"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|connector| {
            let id = connector["id"].as_str().filter(|id| !id.is_empty())?;
            let platform = connector["platform"].as_str().unwrap_or_default();
            Some((id.to_owned(), platform.to_owned()))
        })
        .collect()
}

/// The values of a domain matching what is typed: chats from the directory
/// files of the listed connectors, title first; the listed connectors.
fn suggest(directory: Option<&Path>, query: &Query) -> Result<Vec<Suggestion>, String> {
    let connectors = connectors(query.values);
    match query.domain {
        domains::CONNECTOR => {
            if connectors.is_empty() {
                return Err("No connectors yet: add one under Connectors.".into());
            }
            let text = query.text.trim().to_lowercase();
            Ok(connectors
                .into_iter()
                .filter(|(id, _)| id.to_lowercase().contains(&text))
                .take(query.limit)
                .map(|(id, platform)| Suggestion {
                    label: format!("{id} ({platform})"),
                    value: id,
                })
                .collect())
        }
        domains::CHAT => chats(directory, &connectors, query),
        _ => Ok(Vec::new()),
    }
}

fn chats(
    directory: Option<&Path>,
    connectors: &[(String, String)],
    query: &Query,
) -> Result<Vec<Suggestion>, String> {
    let Some(directory) = directory else {
        return Err(format!(
            "No chat directory: set {} (or $XDG_STATE_HOME or $HOME).",
            wuphf_model::CHAT_DIRECTORY_ENV
        ));
    };
    if connectors.is_empty() {
        return Err("No connectors yet: add one under Connectors.".into());
    }
    let mut found = Vec::new();
    let mut unlinked = Vec::new();
    let mut problems = Vec::new();
    for (id, platform) in connectors {
        match ChatDirectory::read(directory, &ConnectorId(id.clone())) {
            Ok(Some(chats)) => found.extend(
                chats
                    .chats
                    .into_iter()
                    .filter(|chat| chat.matches(query.text))
                    .map(|chat| Suggestion {
                        value: chat.chat.to_string(),
                        label: format!("{} ({}, {})", chat.title, chat.kind, chat.chat),
                    }),
            ),
            Ok(None) => unlinked.push(format!("wuphf-{platform} setup")),
            Err(error) => problems.push(error.to_string()),
        }
        if found.len() >= query.limit {
            break;
        }
    }
    found.truncate(query.limit);
    // Matches, or a search of the chats some connector has: no matches.
    let searched = unlinked.len() + problems.len() < connectors.len();
    if !found.is_empty() || (searched && !query.text.trim().is_empty()) {
        return Ok(found);
    }
    // Nothing to offer: say how to get some.
    if let Some(problem) = problems.into_iter().next() {
        return Err(problem);
    }
    if unlinked.is_empty() {
        return Ok(found);
    }
    unlinked.dedup();
    Err(format!(
        "No chats known yet: link the connectors in their setup ({}, then Link account).",
        unlinked.join(", ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wuphf_model::{ChatRef, DirectoryChat, DirectoryChatKind};

    fn temporary() -> PathBuf {
        let path = std::env::temp_dir().join(format!("wuphf-setup-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn write(directory: &Path, connector: &str, chats: &[(&str, &str)]) {
        ChatDirectory::new(
            ConnectorId(connector.into()),
            "whatsapp",
            1,
            chats
                .iter()
                .map(|(chat, title)| DirectoryChat {
                    chat: format!("{connector}:{chat}").parse::<ChatRef>().unwrap(),
                    title: (*title).into(),
                    kind: DirectoryChatKind::Group,
                })
                .collect(),
        )
        .write(directory)
        .unwrap();
    }

    fn values() -> Value {
        json!({"connectors": [
            {"id": "whatsapp-main", "platform": "whatsapp"},
            {"id": "telegram-main", "platform": "telegram"},
        ]})
    }

    fn ask(
        directory: Option<&Path>,
        domain: &str,
        text: &str,
        values: &Value,
    ) -> Result<Vec<String>, String> {
        let query = Query {
            domain,
            text,
            values,
            limit: 8,
        };
        suggest(directory, &query).map(|found| {
            found
                .into_iter()
                .map(|suggestion| suggestion.label)
                .collect()
        })
    }

    #[test]
    fn chats_come_from_the_listed_connectors_files() {
        let directory = temporary();
        write(
            &directory,
            "whatsapp-main",
            &[("1@g.us", "Family"), ("2@g.us", "Work")],
        );
        // A connector the config does not list is left out.
        write(&directory, "whatsapp-old", &[("3@g.us", "Family, old")]);
        let values = values();
        assert_eq!(
            ask(Some(&directory), domains::CHAT, "fam", &values),
            Ok(vec!["Family (group, whatsapp-main:1@g.us)".to_owned()])
        );
        assert_eq!(
            ask(Some(&directory), domains::CHAT, "", &values).map(|found| found.len()),
            Ok(2)
        );
        assert_eq!(
            ask(Some(&directory), domains::CHAT, "nothing", &values),
            Ok(vec![])
        );
    }

    #[test]
    fn with_no_files_it_says_how_to_get_them() {
        let directory = temporary();
        let error = ask(Some(&directory), domains::CHAT, "", &values()).unwrap_err();
        assert_eq!(
            error,
            "No chats known yet: link the connectors in their setup (wuphf-whatsapp setup, \
             wuphf-telegram setup, then Link account)."
        );
        assert!(
            ask(Some(&directory), domains::CHAT, "", &json!({}))
                .unwrap_err()
                .starts_with("No connectors yet")
        );
        assert!(
            ask(None, domains::CHAT, "", &values())
                .unwrap_err()
                .starts_with("No chat directory")
        );
        std::fs::write(directory.join("whatsapp-main.chats.json"), "{").unwrap();
        assert!(
            ask(Some(&directory), domains::CHAT, "", &values())
                .unwrap_err()
                .contains("is not a chat directory")
        );
    }

    #[test]
    fn connectors_come_from_the_config_being_edited() {
        assert_eq!(
            ask(None, domains::CONNECTOR, "tele", &values()),
            Ok(vec!["telegram-main (telegram)".to_owned()])
        );
        assert_eq!(ask(None, "other", "", &values()), Ok(vec![]));
    }
}
