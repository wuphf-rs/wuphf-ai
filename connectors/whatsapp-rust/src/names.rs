//! Chat titles for mirror threads, formatted like the Go connector's
//! `ChatNamer` (`connectors/whatsapp/internal/whatsapp/names.go`): a `[G]` or
//! `[P]` tag, then the group subject or the person's name.
//!
//! Saved address-book names are not available through this client, so a
//! saved contact shows its push name and number where the Go connector shows
//! the saved name.

use std::sync::Arc;

use whatsapp_rust::client::Client;
use whatsapp_rust::types::message::MessageInfo;
use whatsapp_rust::{Jid, Server};

use crate::groups::GroupDirectory;

/// Resolves the title the core names a chat's mirror thread with. Only group
/// subjects are cached (in the shared [`GroupDirectory`]); a person's push
/// name rides on every message they send.
pub struct ChatTitles {
    groups: Arc<GroupDirectory>,
}

impl ChatTitles {
    pub fn new(groups: Arc<GroupDirectory>) -> Self {
        Self { groups }
    }

    /// The chat's title, or `None` when this message cannot name the chat
    /// (the core then keeps the title it has).
    pub async fn title(&self, client: &Client, info: &MessageInfo) -> Option<String> {
        let chat = info.source.chat.to_non_ad();
        match chat.server {
            Server::Group => {
                let facts = self.groups.facts(client, &chat).await?;
                tagged("G", &facts.subject)
            }
            Server::Pn | Server::Lid => {
                // Only the other party's message names a DM, so the owner's
                // own needs no number lookup.
                let is_from_me = info.source.is_from_me;
                let number = if is_from_me {
                    String::new()
                } else {
                    phone_number(client, &chat).await
                };
                dm_title(is_from_me, &number, &info.push_name)
            }
            _ => None,
        }
    }
}

/// The phone number of a DM's chat: a LID chat is mapped to its phone number
/// when the mapping is known, otherwise the JID's user part is used.
async fn phone_number(client: &Client, chat: &Jid) -> String {
    if chat.is_lid()
        && let Ok(Some(entry)) = client.get_lid_pn_entry(chat).await
    {
        return entry.phone_number.to_string();
    }
    chat.user.to_string()
}

/// A DM's title. The owner's own message names nothing: the push name on it is
/// the owner's, not the chat's, and any fallback (a cache, the number) would
/// rename the thread after a restart. The core keeps the title it stored.
pub fn dm_title(is_from_me: bool, number: &str, push: &str) -> Option<String> {
    if is_from_me {
        return None;
    }
    tagged("P", &format_person_name(number, push))
}

/// Prefix a chat title with its kind (`[G]` group, `[P]` private) so a mirror
/// thread shows what it bridges. An empty title names nothing.
pub fn tagged(kind: &str, title: &str) -> Option<String> {
    let title = title.trim();
    (!title.is_empty()).then(|| format!("[{kind}] {title}"))
}

/// A person by push name with the number in brackets, or the number alone
/// when no push name is known. No `+`, as in the Go connector.
pub fn format_person_name(number: &str, push: &str) -> String {
    let push = push.trim();
    if push.is_empty() {
        number.to_owned()
    } else {
        format!("{push} ({number})")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_person_is_named_by_push_name_and_number() {
        assert_eq!(format_person_name("1555", "Alice"), "Alice (1555)");
        assert_eq!(format_person_name("1555", "  "), "1555");
        assert_eq!(format_person_name("1555", ""), "1555");
    }

    #[test]
    fn an_empty_title_is_not_tagged() {
        assert_eq!(tagged("G", ""), None);
        assert_eq!(
            tagged("P", "Alice (1555)").as_deref(),
            Some("[P] Alice (1555)")
        );
        assert_eq!(tagged("G", "Family").as_deref(), Some("[G] Family"));
    }

    #[test]
    fn only_the_other_party_names_a_dm() {
        assert_eq!(
            dm_title(false, "1555", "Alice").as_deref(),
            Some("[P] Alice (1555)")
        );
        assert_eq!(dm_title(false, "1555", "").as_deref(), Some("[P] 1555"));
        // Never "Me", never the owner's push name, never the number.
        assert_eq!(dm_title(true, "1555", "Owner"), None);
        assert_eq!(dm_title(true, "1555", ""), None);
    }
}
