//! Test doubles shared by the connector's test modules.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};

use whatsapp_rust::Jid;

use crate::identity::LidStore;

/// The account's LID store: phone ↔ LID mappings by user part, counting the
/// lookups made.
#[derive(Default)]
pub struct FakeAccount {
    /// Phone user part → LID user part.
    lids: HashMap<String, String>,
    lookups: AtomicUsize,
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
