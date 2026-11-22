//! Test doubles shared by the connector's test modules.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};

use whatsapp_rust::Jid;
use whatsapp_rust::types::message::AddressingMode;

use crate::addressing::Account;
use crate::identity::LidStore;

/// The paired account and its LID store: phone ↔ LID mappings by user part,
/// the account's own addresses, whether it is LID-migrated and how its groups
/// are addressed, counting the lookups made.
#[derive(Default)]
pub struct FakeAccount {
    /// Phone user part → LID user part.
    pub lids: HashMap<String, String>,
    pub own_pn: Option<Jid>,
    pub own_lid: Option<Jid>,
    pub migrated: bool,
    /// Group JID → its addressing mode; a group missing here fails to look up.
    pub group_modes: HashMap<String, AddressingMode>,
    pub lookups: AtomicUsize,
    pub group_lookups: AtomicUsize,
}

impl FakeAccount {
    /// An account that knows one mapping.
    pub fn mapping(pn: &str, lid: &str) -> Self {
        Self {
            lids: HashMap::from([(pn.to_owned(), lid.to_owned())]),
            ..Self::default()
        }
    }

    pub fn lookups(&self) -> usize {
        self.lookups.load(Ordering::SeqCst)
    }

    pub fn group_lookups(&self) -> usize {
        self.group_lookups.load(Ordering::SeqCst)
    }
}

impl LidStore for FakeAccount {
    async fn lid_for_pn(&self, pn: &Jid) -> Option<Jid> {
        self.lookups.fetch_add(1, Ordering::SeqCst);
        let lid = self.lids.get(pn.user.as_str())?;
        format!("{lid}@lid").parse().ok()
    }

    async fn pn_for_lid(&self, lid: &Jid) -> Option<Jid> {
        self.lookups.fetch_add(1, Ordering::SeqCst);
        let (pn, _) = self
            .lids
            .iter()
            .find(|(_, known)| known.as_str() == lid.user.as_str())?;
        format!("{pn}@s.whatsapp.net").parse().ok()
    }
}

impl Account for FakeAccount {
    fn own_pn(&self) -> Option<Jid> {
        self.own_pn.clone()
    }

    fn own_lid(&self) -> Option<Jid> {
        self.own_lid.clone()
    }

    async fn lid_migrated(&self) -> bool {
        self.migrated
    }

    async fn group_mode(&self, group: &Jid) -> Option<AddressingMode> {
        self.group_lookups.fetch_add(1, Ordering::SeqCst);
        self.group_modes.get(&group.to_string()).copied()
    }
}
