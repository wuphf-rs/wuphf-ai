//! One stable identity per WhatsApp peer, as the Go connector's `identity.go`
//! computes it. WhatsApp names one person by a phone JID, a LID and per-device
//! forms of either, switching from message to message; the core keys an
//! endpoint (and so a mirror thread) on the chat id, so each form left
//! unmapped would split a chat. Both connectors canonicalize the same way, so
//! a connector id can move between them without splitting conversations.
//!
//! Ids on the wire go through [`Identity::canonical`]; `bare_id` (the device
//! stripped, nothing else) is only for the connector's own tables.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::{Mutex, RwLock};

use tracing::debug;
use whatsapp_rust::client::Client;
use whatsapp_rust::types::message::{AddressingMode, MessageSource};
use whatsapp_rust::{Jid, Server};
use wuphf_proto::connector as pb;

/// Phone-number ↔ LID lookups in the client's own store (no network).
pub trait LidStore: Send + Sync {
    fn lid_for_pn(&self, pn: &Jid) -> impl Future<Output = Option<Jid>> + Send;
    fn pn_for_lid(&self, lid: &Jid) -> impl Future<Output = Option<Jid>> + Send;
}

impl LidStore for Client {
    async fn lid_for_pn(&self, pn: &Jid) -> Option<Jid> {
        let entry = mapping(self, pn).await?;
        format!("{}@lid", entry.lid).parse().ok()
    }

    async fn pn_for_lid(&self, lid: &Jid) -> Option<Jid> {
        let entry = mapping(self, lid).await?;
        format!("{}@s.whatsapp.net", entry.phone_number)
            .parse()
            .ok()
    }
}

/// The client's mapping entry for `jid`; a lookup error is a miss.
async fn mapping(
    client: &Client,
    jid: &Jid,
) -> Option<whatsapp_rust::wacore::types::lid_pn::LidPnEntry> {
    match client.get_lid_pn_entry(jid).await {
        Ok(entry) => entry,
        Err(error) => {
            debug!(%error, %jid, "WhatsApp LID mapping lookup failed");
            None
        }
    }
}

/// Canonical ids, with the hits cached for the process (a mapping is stable for
/// the life of a contact); a miss is not cached, as the mapping may be learned
/// later and must then take effect.
#[derive(Default)]
pub struct Identity {
    /// Phone → LID.
    lids: RwLock<HashMap<String, Jid>>,
    /// LID → phone.
    pns: RwLock<HashMap<String, Jid>>,
}

impl Identity {
    /// The device stripped; a phone JID promoted to its LID when known; any
    /// other server unchanged.
    pub async fn canonical(&self, store: &impl LidStore, jid: &Jid) -> Jid {
        let jid = jid.to_non_ad();
        if jid.server != Server::Pn {
            return jid;
        }
        match self.lid(store, &jid).await {
            Some(lid) => lid,
            None => jid,
        }
    }

    /// [`Self::canonical`] as the id frames carry.
    pub async fn canonical_id(&self, store: &impl LidStore, jid: &Jid) -> String {
        self.canonical(store, jid).await.to_string()
    }

    /// The other form of a user JID (LID ↔ phone), when known. Groups,
    /// channels and broadcasts have one form only.
    pub async fn alternate(&self, store: &impl LidStore, jid: &Jid) -> Option<Jid> {
        let jid = jid.to_non_ad();
        match jid.server {
            Server::Pn => self.lid(store, &jid).await,
            Server::Lid => self.pn(store, &jid).await,
            _ => None,
        }
    }

    /// `id` canonical when it parses as a JID; unchanged otherwise.
    pub async fn canonical_str(&self, store: &impl LidStore, id: &str) -> String {
        match id.parse::<Jid>() {
            Ok(jid) => self.canonical_id(store, &jid).await,
            Err(_) => id.to_owned(),
        }
    }

    /// Chat updates as the core is told them (Go reports chat updates under
    /// the canonical id only): every chat, participant and actor id
    /// canonical, and one update per chat, the first, so a chat a producer
    /// reported under two forms is reported once. Once the forms are linked,
    /// two reports of one change would be noted twice.
    pub async fn canonical_updates(
        &self,
        store: &impl LidStore,
        updates: Vec<pb::ChatUpdate>,
    ) -> Vec<pb::ChatUpdate> {
        let mut seen = HashSet::new();
        let mut canonical = Vec::with_capacity(updates.len());
        for mut update in updates {
            update.chat_id = self.canonical_str(store, &update.chat_id).await;
            if !seen.insert(update.chat_id.clone()) {
                continue;
            }
            for change in &mut update.participant_changes {
                change.participant = self.canonical_str(store, &change.participant).await;
                if let Some(actor) = &change.actor {
                    change.actor = Some(self.canonical_str(store, actor).await);
                }
            }
            canonical.push(update);
        }
        canonical
    }

    async fn lid(&self, store: &impl LidStore, pn: &Jid) -> Option<Jid> {
        let key = pn.to_string();
        if let Some(hit) = self.lids.read().unwrap().get(&key) {
            return Some(hit.clone());
        }
        let found = store.lid_for_pn(pn).await?;
        self.lids.write().unwrap().insert(key, found.clone());
        Some(found)
    }

    async fn pn(&self, store: &impl LidStore, lid: &Jid) -> Option<Jid> {
        let key = lid.to_string();
        if let Some(hit) = self.pns.read().unwrap().get(&key) {
            return Some(hit.clone());
        }
        let found = store.pn_for_lid(lid).await?;
        self.pns.write().unwrap().insert(key, found.clone());
        Some(found)
    }
}

/// WhatsApp's own hint that a DM's address changed (Go `identityMigration`):
/// the other party's alt address is `sender_alt` on their messages and
/// `recipient_alt` on the account's own; the LID form is the new one. Groups
/// get none: a participant's pair is not a chat migration.
pub fn identity_migration(source: &MessageSource) -> Option<pb::IdentityMigrated> {
    if source.is_group {
        return None;
    }
    let alt = if source.is_from_me {
        source.recipient_alt.as_ref()
    } else {
        source.sender_alt.as_ref()
    }?
    .to_non_ad();
    let chat = source.chat.to_non_ad();
    let (old, new) = match source.addressing_mode {
        Some(AddressingMode::Lid) => (alt, chat),
        _ => (chat, alt),
    };
    (old != new).then(|| migration(&old, &new))
}

/// Every migration a DM implies (Go `migrationsFor`, before the gate): the
/// hint above; the raw chat when it differs from the canonical `chat`; and the
/// `alternate` form → the canonical chat. Nothing for a group.
pub fn migrations_for(
    source: &MessageSource,
    chat: &Jid,
    alternate: Option<&Jid>,
) -> Vec<pb::IdentityMigrated> {
    if source.is_group {
        return Vec::new();
    }
    let mut migrations: Vec<_> = identity_migration(source).into_iter().collect();
    let raw = source.chat.to_non_ad();
    if raw != *chat {
        migrations.push(migration(&raw, chat));
    }
    if let Some(alternate) = alternate.filter(|alternate| *alternate != chat) {
        migrations.push(migration(alternate, chat));
    }
    migrations
}

fn migration(old: &Jid, new: &Jid) -> pb::IdentityMigrated {
    pb::IdentityMigrated {
        old_chat_id: old.to_string(),
        new_chat_id: new.to_string(),
        ..Default::default()
    }
}

/// Drops migrations already reported by this process (Go `migrationGate`):
/// the core's migration is idempotent, but a chat would repeat the same pair
/// on every message.
#[derive(Default)]
pub struct MigrationGate {
    seen: Mutex<HashSet<(String, String)>>,
}

impl MigrationGate {
    pub fn unseen(&self, migrations: Vec<pb::IdentityMigrated>) -> Vec<pb::IdentityMigrated> {
        let mut seen = self.seen.lock().unwrap();
        migrations
            .into_iter()
            .filter(|migration| {
                seen.insert((migration.old_chat_id.clone(), migration.new_chat_id.clone()))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FakeAccount;

    fn jid(raw: &str) -> Jid {
        raw.parse().unwrap()
    }

    fn source(chat: &str) -> MessageSource {
        MessageSource {
            chat: jid(chat),
            sender: jid(chat),
            ..Default::default()
        }
    }

    fn pairs(migrations: &[pb::IdentityMigrated]) -> Vec<(&str, &str)> {
        migrations
            .iter()
            .map(|migration| {
                (
                    migration.old_chat_id.as_str(),
                    migration.new_chat_id.as_str(),
                )
            })
            .collect()
    }

    #[tokio::test]
    async fn a_phone_number_becomes_its_lid() {
        let account = FakeAccount::mapping("1555", "9");
        let identity = Identity::default();
        assert_eq!(
            identity
                .canonical(&account, &jid("1555:3@s.whatsapp.net"))
                .await,
            jid("9@lid")
        );
        // No mapping: the device is still stripped.
        assert_eq!(
            identity
                .canonical(&account, &jid("1777:2@s.whatsapp.net"))
                .await,
            jid("1777@s.whatsapp.net")
        );
    }

    #[tokio::test]
    async fn hits_are_cached_misses_are_not() {
        let account = FakeAccount::mapping("1555", "9");
        let identity = Identity::default();
        identity
            .canonical(&account, &jid("1555@s.whatsapp.net"))
            .await;
        identity
            .canonical(&account, &jid("1555@s.whatsapp.net"))
            .await;
        assert_eq!(account.lookups(), 1);
        identity
            .canonical(&account, &jid("1777@s.whatsapp.net"))
            .await;
        identity
            .canonical(&account, &jid("1777@s.whatsapp.net"))
            .await;
        assert_eq!(account.lookups(), 3);
    }

    #[tokio::test]
    async fn groups_lids_and_channels_stay_as_they_are() {
        let account = FakeAccount::mapping("1555", "9");
        let identity = Identity::default();
        for raw in ["123@g.us", "9@lid", "1@newsletter"] {
            assert_eq!(identity.canonical(&account, &jid(raw)).await, jid(raw));
        }
        assert_eq!(account.lookups(), 0);
    }

    #[tokio::test]
    async fn the_alternate_maps_both_ways() {
        let account = FakeAccount::mapping("1555", "9");
        let identity = Identity::default();
        assert_eq!(
            identity
                .alternate(&account, &jid("1555@s.whatsapp.net"))
                .await,
            Some(jid("9@lid"))
        );
        assert_eq!(
            identity.alternate(&account, &jid("9@lid")).await,
            Some(jid("1555@s.whatsapp.net"))
        );
        assert_eq!(identity.alternate(&account, &jid("123@g.us")).await, None);
        assert_eq!(
            identity
                .alternate(&account, &jid("1777@s.whatsapp.net"))
                .await,
            None
        );
    }

    #[test]
    fn a_recipient_alt_makes_the_lid_canonical() {
        let mut own = MessageSource {
            is_from_me: true,
            addressing_mode: Some(AddressingMode::Lid),
            recipient_alt: Some(jid("1555@s.whatsapp.net")),
            ..source("9@lid")
        };
        let hint = identity_migration(&own).unwrap();
        assert_eq!(
            (hint.old_chat_id.as_str(), hint.new_chat_id.as_str()),
            ("1555@s.whatsapp.net", "9@lid")
        );
        own.chat = jid("1555@s.whatsapp.net");
        own.addressing_mode = Some(AddressingMode::Pn);
        own.recipient_alt = Some(jid("9@lid"));
        let hint = identity_migration(&own).unwrap();
        assert_eq!(
            (hint.old_chat_id.as_str(), hint.new_chat_id.as_str()),
            ("1555@s.whatsapp.net", "9@lid")
        );
    }

    #[test]
    fn an_incoming_dm_uses_the_sender_alt() {
        let theirs = MessageSource {
            addressing_mode: Some(AddressingMode::Lid),
            sender_alt: Some(jid("1555@s.whatsapp.net")),
            ..source("9@lid")
        };
        let hint = identity_migration(&theirs).unwrap();
        assert_eq!(hint.old_chat_id, "1555@s.whatsapp.net");
        // On our own message the recipient's alt is the one that names the
        // other party.
        let ours = MessageSource {
            is_from_me: true,
            sender_alt: Some(jid("1999@s.whatsapp.net")),
            recipient_alt: Some(jid("1555@s.whatsapp.net")),
            ..theirs
        };
        assert_eq!(
            identity_migration(&ours).unwrap().old_chat_id,
            "1555@s.whatsapp.net"
        );
    }

    #[test]
    fn no_hint_for_groups_or_without_an_alt() {
        let group = MessageSource {
            is_group: true,
            sender_alt: Some(jid("1555@s.whatsapp.net")),
            ..source("123@g.us")
        };
        assert!(identity_migration(&group).is_none());
        assert!(migrations_for(&group, &jid("123@g.us"), None).is_empty());
        assert!(identity_migration(&source("9@lid")).is_none());
    }

    #[test]
    fn migrations_cover_the_shift_and_the_alternate() {
        // A raw phone chat whose LID is known.
        let raw = source("1555@s.whatsapp.net");
        assert_eq!(
            pairs(&migrations_for(&raw, &jid("9@lid"), None)),
            [("1555@s.whatsapp.net", "9@lid")]
        );
        // A LID chat with a known phone.
        let lid = source("9@lid");
        let phone = jid("1555@s.whatsapp.net");
        assert_eq!(
            pairs(&migrations_for(&lid, &jid("9@lid"), Some(&phone))),
            [("1555@s.whatsapp.net", "9@lid")]
        );
    }

    #[tokio::test]
    async fn chat_updates_go_out_canonical_and_once_per_chat() {
        let account = FakeAccount::mapping("1555", "9");
        let identity = Identity::default();
        let update = |chat_id: &str, title: &str| pb::ChatUpdate {
            chat_id: chat_id.into(),
            title: Some(title.into()),
            ..Default::default()
        };
        let group = pb::ChatUpdate {
            participant_changes: vec![pb::ParticipantChange {
                participant: "1555:2@s.whatsapp.net".into(),
                actor: Some("1555@s.whatsapp.net".into()),
                ..Default::default()
            }],
            ..update("123@g.us", "[G] Family")
        };
        let updates = identity
            .canonical_updates(
                &account,
                vec![
                    update("1555@s.whatsapp.net", "[P] Alice"),
                    // The same DM under its other form: once is enough.
                    update("9@lid", "[P] Alice"),
                    group,
                ],
            )
            .await;
        let ids: Vec<_> = updates
            .iter()
            .map(|update| update.chat_id.as_str())
            .collect();
        assert_eq!(ids, ["9@lid", "123@g.us"]);
        let change = &updates[1].participant_changes[0];
        assert_eq!(change.participant, "9@lid");
        assert_eq!(change.actor.as_deref(), Some("9@lid"));
    }

    #[test]
    fn each_migration_is_reported_once() {
        let gate = MigrationGate::default();
        let raw = source("1555@s.whatsapp.net");
        let first = gate.unseen(migrations_for(&raw, &jid("9@lid"), None));
        assert_eq!(first.len(), 1);
        assert!(
            gate.unseen(migrations_for(&raw, &jid("9@lid"), None))
                .is_empty()
        );
    }
}
