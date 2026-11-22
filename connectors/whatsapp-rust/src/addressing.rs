//! The address form (phone JID or LID) each chat is addressed in, as the Go
//! connector's `addressing.go` records it. Dispatches carry canonical ids, but
//! WhatsApp keys a chat's messages by the chat's own form, so a quote naming a
//! person in the other form does not link to the message it means. This
//! records the form each chat arrives in and translates an address back to it
//! before it goes on the wire.

use std::collections::HashMap;
use std::future::Future;
use std::sync::RwLock;

use whatsapp_rust::client::Client;
use whatsapp_rust::types::message::{AddressingMode, MessageSource};
use whatsapp_rust::{Jid, Server};

use crate::identity::{Identity, LidStore};

/// The paired account and WhatsApp's view of it (Go `accountIdentity`).
pub trait Account: Send + Sync {
    fn own_pn(&self) -> Option<Jid>;
    fn own_lid(&self) -> Option<Jid>;
    /// Whether the account moved its DMs to LIDs (the library then sends every
    /// DM to the LID).
    fn lid_migrated(&self) -> impl Future<Output = bool> + Send;
    /// How WhatsApp addresses a group; `None` when the lookup fails.
    fn group_mode(&self, group: &Jid) -> impl Future<Output = Option<AddressingMode>> + Send;
}

impl Account for Client {
    fn own_pn(&self) -> Option<Jid> {
        self.get_pn()
    }

    fn own_lid(&self) -> Option<Jid> {
        self.get_lid()
    }

    async fn lid_migrated(&self) -> bool {
        self.is_lid_migrated().await
    }

    async fn group_mode(&self, group: &Jid) -> Option<AddressingMode> {
        let metadata = self.groups().get_metadata(group).await.ok()?;
        Some(metadata.addressing_mode)
    }
}

/// The form each chat is addressed in, by canonical chat id. A chat's form
/// comes from the last message that arrived in it, then from WhatsApp itself
/// (a group's metadata, or for a DM whether the account is LID-migrated), so a
/// chat that has not spoken since a restart is still addressed right.
#[derive(Default)]
pub struct AddressBook {
    modes: RwLock<HashMap<String, AddressingMode>>,
}

impl AddressBook {
    /// Record the form a message's chat arrived in, under `chat_id`, its
    /// canonical id (the one dispatches name): a group states its form on every
    /// message; a DM's form is the server its raw address is on; channels and
    /// broadcasts have one form and are not recorded.
    pub fn observe(&self, chat_id: &str, source: &MessageSource) {
        let mode = if source.is_group {
            match source.addressing_mode {
                Some(AddressingMode::Lid) => AddressingMode::Lid,
                _ => AddressingMode::Pn,
            }
        } else {
            match source.chat.server {
                Server::Lid => AddressingMode::Lid,
                Server::Pn => AddressingMode::Pn,
                _ => return,
            }
        };
        self.modes.write().unwrap().insert(chat_id.to_owned(), mode);
    }

    /// The form `chat` is addressed in. A DM: the LID when the account is
    /// migrated, else the recorded form, else its server's. A group: the
    /// recorded form, else WhatsApp's, asked once and remembered. `None` when
    /// unknown: addresses are then left as they are rather than guessed.
    pub async fn mode(&self, account: &impl Account, chat: &Jid) -> Option<AddressingMode> {
        let chat = chat.to_non_ad();
        let key = chat.to_string();
        let recorded = self.modes.read().unwrap().get(&key).copied();
        match chat.server {
            Server::Pn | Server::Lid => {
                if account.lid_migrated().await {
                    return Some(AddressingMode::Lid);
                }
                Some(recorded.unwrap_or(match chat.server {
                    Server::Lid => AddressingMode::Lid,
                    _ => AddressingMode::Pn,
                }))
            }
            Server::Group => {
                if recorded.is_some() {
                    return recorded;
                }
                let mode = match account.group_mode(&chat).await? {
                    AddressingMode::Lid => AddressingMode::Lid,
                    _ => AddressingMode::Pn,
                };
                self.modes.write().unwrap().insert(key, mode);
                Some(mode)
            }
            _ => None,
        }
    }

    /// `jid` in `mode` when its counterpart is known, else unchanged (groups
    /// and other non-user addresses have one form).
    pub async fn in_mode(
        &self,
        identity: &Identity,
        store: &impl LidStore,
        jid: &Jid,
        mode: Option<AddressingMode>,
    ) -> Jid {
        let jid = jid.to_non_ad();
        let translate = matches!(
            (mode, jid.server),
            (Some(AddressingMode::Lid), Server::Pn) | (Some(AddressingMode::Pn), Server::Lid)
        );
        if translate && let Some(other) = identity.alternate(store, &jid).await {
            return other;
        }
        jid
    }

    /// The paired account in `mode`: its LID in LID mode when known, else its
    /// phone JID (device stripped).
    pub fn own(&self, account: &impl Account, mode: Option<AddressingMode>) -> Option<Jid> {
        if mode == Some(AddressingMode::Lid)
            && let Some(lid) = account.own_lid()
        {
            return Some(lid.to_non_ad());
        }
        account.own_pn().map(|pn| pn.to_non_ad())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FakeAccount;

    fn jid(raw: &str) -> Jid {
        raw.parse().unwrap()
    }

    fn group_source(mode: AddressingMode) -> MessageSource {
        MessageSource {
            chat: jid("123@g.us"),
            is_group: true,
            addressing_mode: Some(mode),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn a_group_s_mode_comes_from_its_messages() {
        let account = FakeAccount::default();
        let book = AddressBook::default();
        book.observe("123@g.us", &group_source(AddressingMode::Lid));
        assert_eq!(
            book.mode(&account, &jid("123@g.us")).await,
            Some(AddressingMode::Lid)
        );
        assert_eq!(account.group_lookups(), 0);
    }

    #[tokio::test]
    async fn a_group_s_mode_is_asked_once() {
        let account = FakeAccount {
            group_modes: HashMap::from([("123@g.us".into(), AddressingMode::Lid)]),
            ..FakeAccount::default()
        };
        let book = AddressBook::default();
        for _ in 0..2 {
            assert_eq!(
                book.mode(&account, &jid("123@g.us")).await,
                Some(AddressingMode::Lid)
            );
        }
        assert_eq!(account.group_lookups(), 1);
    }

    #[tokio::test]
    async fn an_unknown_group_mode_keeps_addresses() {
        let account = FakeAccount::mapping("1555", "9");
        let book = AddressBook::default();
        let mode = book.mode(&account, &jid("456@g.us")).await;
        assert_eq!(mode, None);
        let identity = Identity::default();
        assert_eq!(
            book.in_mode(&identity, &account, &jid("1555@s.whatsapp.net"), mode)
                .await,
            jid("1555@s.whatsapp.net")
        );
    }

    #[tokio::test]
    async fn a_direct_chat_s_mode() {
        let migrated = FakeAccount {
            migrated: true,
            ..FakeAccount::default()
        };
        let book = AddressBook::default();
        book.observe(
            "1555@s.whatsapp.net",
            &MessageSource {
                chat: jid("1555@s.whatsapp.net"),
                ..Default::default()
            },
        );
        assert_eq!(
            book.mode(&migrated, &jid("1555@s.whatsapp.net")).await,
            Some(AddressingMode::Lid)
        );
        let account = FakeAccount::default();
        assert_eq!(
            book.mode(&account, &jid("1555@s.whatsapp.net")).await,
            Some(AddressingMode::Pn)
        );
        assert_eq!(
            book.mode(&account, &jid("9@lid")).await,
            Some(AddressingMode::Lid)
        );
        assert_eq!(
            book.mode(&account, &jid("1777@s.whatsapp.net")).await,
            Some(AddressingMode::Pn)
        );
    }

    #[tokio::test]
    async fn known_counterparts_are_translated() {
        let account = FakeAccount::mapping("1555", "9");
        let identity = Identity::default();
        let book = AddressBook::default();
        let lid = Some(AddressingMode::Lid);
        let pn = Some(AddressingMode::Pn);
        assert_eq!(
            book.in_mode(&identity, &account, &jid("9@lid"), pn).await,
            jid("1555@s.whatsapp.net")
        );
        assert_eq!(
            book.in_mode(&identity, &account, &jid("1555:2@s.whatsapp.net"), lid)
                .await,
            jid("9@lid")
        );
        assert_eq!(
            book.in_mode(&identity, &account, &jid("1777@s.whatsapp.net"), lid)
                .await,
            jid("1777@s.whatsapp.net")
        );
        assert_eq!(
            book.in_mode(&identity, &account, &jid("123@g.us"), lid)
                .await,
            jid("123@g.us")
        );
    }

    #[test]
    fn our_own_address_follows_the_mode() {
        let account = FakeAccount {
            own_pn: Some(jid("1999:4@s.whatsapp.net")),
            own_lid: Some(jid("7:4@lid")),
            ..FakeAccount::default()
        };
        let book = AddressBook::default();
        assert_eq!(
            book.own(&account, Some(AddressingMode::Lid)),
            Some(jid("7@lid"))
        );
        assert_eq!(
            book.own(&account, Some(AddressingMode::Pn)),
            Some(jid("1999@s.whatsapp.net"))
        );
        let without_lid = FakeAccount {
            own_lid: None,
            ..account
        };
        assert_eq!(
            book.own(&without_lid, Some(AddressingMode::Lid)),
            Some(jid("1999@s.whatsapp.net"))
        );
    }

    #[tokio::test]
    async fn the_normalizer_records_each_chat_s_form() {
        let account = FakeAccount::default();
        let book = AddressBook::default();
        book.observe("123@g.us", &group_source(AddressingMode::Lid));
        book.observe("456@g.us", &group_source(AddressingMode::Pn));
        book.observe(
            "9@lid",
            &MessageSource {
                chat: jid("9@lid"),
                ..Default::default()
            },
        );
        book.observe(
            "1@newsletter",
            &MessageSource {
                chat: jid("1@newsletter"),
                ..Default::default()
            },
        );
        assert_eq!(
            book.mode(&account, &jid("123@g.us")).await,
            Some(AddressingMode::Lid)
        );
        assert_eq!(
            book.mode(&account, &jid("456@g.us")).await,
            Some(AddressingMode::Pn)
        );
        assert_eq!(
            book.mode(&account, &jid("9@lid")).await,
            Some(AddressingMode::Lid)
        );
        assert!(!book.modes.read().unwrap().contains_key("1@newsletter"));
    }
}
