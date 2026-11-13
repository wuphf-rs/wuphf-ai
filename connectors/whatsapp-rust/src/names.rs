//! Chat titles and person names, formatted like the Go connector's
//! `ChatNamer` (`connectors/whatsapp/internal/whatsapp/names.go`): a `[G]` or
//! `[P]` tag, then the group subject or the person's name.
//!
//! People are named from the connector's own contacts table (saved full
//! names from contact syncs, push names from messages and history syncs).
//! whatsapp-rust has no business-name source, so the order is the Go one
//! without its business step: saved full name, then push name, then number.

use std::sync::Arc;

use whatsapp_rust::client::Client;
use whatsapp_rust::types::message::MessageInfo;
use whatsapp_rust::{Jid, Server};

use crate::chats::{forms_of, lid_pn};
use crate::contacts::{ContactNames, Contacts};
use crate::groups::GroupDirectory;

/// A person as the connector can name them: the phone number (or the id when
/// no number is known) and the names on record.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Person {
    pub number: String,
    pub names: ContactNames,
}

impl Person {
    /// The person's display name ("Alice Smith", "Ali (1555)", "1555").
    pub fn name(&self) -> String {
        format_person_name(&self.number, &self.names)
    }
}

/// Resolves the titles the core names mirror threads with, and the names of
/// people. Group subjects are cached in the shared [`GroupDirectory`]; names
/// come from the [`Contacts`] table.
pub struct ChatTitles {
    groups: Arc<GroupDirectory>,
    contacts: Arc<Contacts>,
}

impl ChatTitles {
    pub fn new(groups: Arc<GroupDirectory>, contacts: Arc<Contacts>) -> Self {
        Self { groups, contacts }
    }

    pub fn contacts(&self) -> &Contacts {
        &self.contacts
    }

    pub fn groups(&self) -> &GroupDirectory {
        &self.groups
    }

    /// The chat's title, or `None` when nothing names the chat yet (the core
    /// then keeps the title it has).
    pub async fn title(&self, client: &Client, info: &MessageInfo) -> Option<String> {
        let chat = info.source.chat.to_non_ad();
        match chat.server {
            Server::Group => {
                let facts = self.groups.facts(client, &chat).await?;
                tagged("G", &facts.subject)
            }
            Server::Pn | Server::Lid => self.dm_title(client, &chat, info.source.is_from_me).await,
            _ => None,
        }
    }

    /// A DM's title from the contacts table. `is_from_me` says the owner's
    /// side is the one speaking, so a bare number is no title.
    pub async fn dm_title(&self, client: &Client, chat: &Jid, is_from_me: bool) -> Option<String> {
        let person = self.person(client, chat, None).await;
        dm_title(is_from_me, &person.number, &person.names)
    }

    /// Look a person up under their JID, its other (phone or LID) form when
    /// the client knows the mapping, and `phone` when the caller has the
    /// phone form at hand (a group notification's participant).
    pub async fn person(&self, client: &Client, jid: &Jid, phone: Option<&Jid>) -> Person {
        let jid = jid.to_non_ad();
        let mapping = lid_pn(client, &jid).await;
        let mapping = mapping
            .as_ref()
            .map(|(lid, pn)| (lid.as_str(), pn.as_str()));
        let mut forms = forms_of(&jid, mapping);
        let phone = phone.map(Jid::to_non_ad);
        if let Some(phone) = &phone {
            let form = phone.to_string();
            if !forms.contains(&form) {
                forms.push(form);
            }
        }
        Person {
            number: number_of(&jid, mapping, phone.as_ref()),
            names: self.contacts.get(forms).await,
        }
    }
}

/// A person's phone number: a phone JID's user part, the mapped number of a
/// LID, the phone form the caller had, or else the JID's user part.
pub fn number_of(jid: &Jid, mapping: Option<(&str, &str)>, phone: Option<&Jid>) -> String {
    if jid.server == Server::Pn {
        return jid.user.to_string();
    }
    if let Some((_, pn)) = mapping {
        return pn.to_owned();
    }
    if let Some(phone) = phone {
        return phone.user.to_string();
    }
    jid.user.to_string()
}

/// A DM's title: the contact's name for either side when a saved or push
/// name is known (so a DM the owner starts is named too); the number when
/// the other party wrote and nothing else is known; otherwise `None`. A
/// title appears only from real data, so a restart never renames a thread
/// to something worse.
pub fn dm_title(is_from_me: bool, number: &str, names: &ContactNames) -> Option<String> {
    if is_from_me && !names.is_named() {
        return None;
    }
    tagged("P", &format_person_name(number, names))
}

/// Prefix a chat title with its kind (`[G]` group, `[P]` private) so a mirror
/// thread shows what it bridges. An empty title names nothing.
pub fn tagged(kind: &str, title: &str) -> Option<String> {
    let title = title.trim();
    (!title.is_empty()).then(|| format!("[{kind}] {title}"))
}

/// A person by descending specificity, like the Go `formatPersonName`
/// without its business step: a saved full name alone; else the push name
/// with the number in brackets; else the number. No `+`, as in Go.
pub fn format_person_name(number: &str, names: &ContactNames) -> String {
    let named = |name: &Option<String>| {
        name.as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
    };
    if let Some(full) = named(&names.full_name) {
        return full;
    }
    match named(&names.push_name) {
        Some(push) => format!("{push} ({number})"),
        None => number.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(full: Option<&str>, push: Option<&str>) -> ContactNames {
        ContactNames {
            full_name: full.map(str::to_owned),
            first_name: None,
            push_name: push.map(str::to_owned),
        }
    }

    #[test]
    fn a_person_is_named_by_full_name_then_push_name_then_number() {
        assert_eq!(
            format_person_name("1555", &names(Some("Alice Smith"), Some("Ali"))),
            "Alice Smith"
        );
        assert_eq!(
            format_person_name("1555", &names(None, Some("Ali"))),
            "Ali (1555)"
        );
        assert_eq!(format_person_name("1555", &names(None, Some("  "))), "1555");
        assert_eq!(format_person_name("1555", &names(Some(" "), None)), "1555");
        assert_eq!(format_person_name("1555", &ContactNames::default()), "1555");
        // A first name alone names nobody.
        let first_only = ContactNames {
            first_name: Some("Alice".into()),
            ..Default::default()
        };
        assert_eq!(format_person_name("1555", &first_only), "1555");
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
    fn a_dm_is_named_from_the_contact_on_either_side() {
        assert_eq!(
            dm_title(false, "1555", &names(Some("Alice Smith"), None)).as_deref(),
            Some("[P] Alice Smith")
        );
        assert_eq!(
            dm_title(false, "1555", &names(None, Some("Ali"))).as_deref(),
            Some("[P] Ali (1555)")
        );
        assert_eq!(
            dm_title(false, "1555", &ContactNames::default()).as_deref(),
            Some("[P] 1555")
        );
        // A DM the owner starts is named after the known contact.
        assert_eq!(
            dm_title(true, "1555", &names(Some("Alice Smith"), None)).as_deref(),
            Some("[P] Alice Smith")
        );
        assert_eq!(
            dm_title(true, "1555", &names(None, Some("Ali"))).as_deref(),
            Some("[P] Ali (1555)")
        );
        // Nothing known: never the bare number from the owner's side.
        assert_eq!(dm_title(true, "1555", &ContactNames::default()), None);
    }

    #[test]
    fn the_number_prefers_the_phone_form() {
        let pn: Jid = "1555@s.whatsapp.net".parse().unwrap();
        let lid: Jid = "8888@lid".parse().unwrap();
        assert_eq!(number_of(&pn, None, None), "1555");
        assert_eq!(number_of(&lid, Some(("8888", "1555")), None), "1555");
        assert_eq!(number_of(&lid, None, Some(&pn)), "1555");
        assert_eq!(number_of(&lid, None, None), "8888");
    }
}
