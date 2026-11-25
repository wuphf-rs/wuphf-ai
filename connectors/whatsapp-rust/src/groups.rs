//! A small cache of group facts the connector needs: the subject that names a
//! mirror thread, the description and member count a first sighting reports,
//! and the group's disappearing-messages timer. Shared by the chat namer, the
//! first-sighting report and the sender, so a group is looked up once per TTL
//! however many messages it carries.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tracing::debug;
use whatsapp_rust::Jid;
use whatsapp_rust::client::Client;
use wuphf_proto::connector as pb;

use crate::community;

/// How long a group's facts are trusted before they are fetched again. Like
/// the Go connector's `titleTTL`: long enough that a busy group is not looked
/// up per message, short enough that a renamed group or a changed timer is
/// picked up without a restart (a group notification also drops the entry).
const GROUP_TTL: Duration = Duration::from_secs(5 * 60);

/// What the connector knows about one group.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GroupFacts {
    pub subject: String,
    /// The group description; empty when the group has none.
    pub description: String,
    /// How many members the group has.
    pub participant_count: u32,
    /// The group's disappearing-messages timer; 0 when off.
    pub disappearing_seconds: u32,
    /// The community the group belongs to (plan 085), when it is in one.
    pub community: Option<Jid>,
}

/// Group facts by canonical group JID, fetched on a miss or after the TTL.
/// The client is passed per call: the whatsapp-rust client exists only once
/// the bot is built, after its handlers are registered.
#[derive(Default)]
pub struct GroupDirectory {
    entries: Mutex<HashMap<String, (GroupFacts, Instant)>>,
}

impl GroupDirectory {
    pub fn new() -> Self {
        Self::default()
    }

    /// The group's facts, from the cache or the network. A failed lookup
    /// returns `None` and caches nothing, so the next call tries again.
    pub async fn facts(&self, client: &Client, jid: &Jid) -> Option<GroupFacts> {
        let key = jid.to_non_ad().to_string();
        if let Some(facts) = self.cached(&key) {
            return Some(facts);
        }
        let metadata = match client.groups().get_metadata(jid).await {
            Ok(metadata) => metadata,
            Err(error) => {
                debug!(group = %key, %error, "WhatsApp group lookup failed");
                return None;
            }
        };
        let participant_count = match metadata.participants.len() {
            0 => metadata.size.unwrap_or(0),
            listed => u32::try_from(listed).unwrap_or(u32::MAX),
        };
        let community = community::parent_of(&metadata).cloned();
        let facts = GroupFacts {
            subject: metadata.subject,
            description: metadata.description.unwrap_or_default(),
            participant_count,
            disappearing_seconds: metadata.ephemeral_expiration,
            community,
        };
        self.entries
            .lock()
            .unwrap()
            .insert(key, (facts.clone(), Instant::now()));
        Some(facts)
    }

    /// The community of a group with `facts`, named from the community's own
    /// facts (an unknown name is empty); `None` for a group in none.
    pub async fn community(&self, client: &Client, facts: &GroupFacts) -> Option<(Jid, String)> {
        let parent = facts.community.clone()?;
        let name = Box::pin(self.facts(client, &parent))
            .await
            .map(|community| community.subject)
            .unwrap_or_default();
        Some((parent, name))
    }

    /// A group's display title (`[G] Community › Group` in a community) and
    /// its community as the core is told it.
    pub async fn identity(
        &self,
        client: &Client,
        jid: &Jid,
    ) -> Option<(Option<String>, pb::ChatCommunity)> {
        let facts = self.facts(client, jid).await?;
        let community = self.community(client, &facts).await;
        let name = community.as_ref().map(|(_, name)| name.as_str());
        Some((
            community::group_title(&facts.subject, name),
            community::reported(community.as_ref().map(|(jid, name)| (jid, name.as_str()))),
        ))
    }

    /// Drop a group's cached facts, after a notification that may have changed
    /// them.
    pub fn forget(&self, jid: &Jid) {
        self.entries
            .lock()
            .unwrap()
            .remove(&jid.to_non_ad().to_string());
    }

    fn cached(&self, key: &str) -> Option<GroupFacts> {
        let entries = self.entries.lock().unwrap();
        let (facts, fetched_at) = entries.get(key)?;
        (fetched_at.elapsed() < GROUP_TTL).then(|| facts.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_entry_is_served_and_a_forgotten_one_is_not() {
        let directory = GroupDirectory::new();
        let jid: Jid = "123@g.us".parse().unwrap();
        let facts = GroupFacts {
            subject: "Family".into(),
            description: "Our family".into(),
            participant_count: 4,
            disappearing_seconds: 604_800,
            community: None,
        };
        directory
            .entries
            .lock()
            .unwrap()
            .insert("123@g.us".into(), (facts.clone(), Instant::now()));
        assert_eq!(directory.cached("123@g.us"), Some(facts));

        directory.forget(&jid);
        assert_eq!(directory.cached("123@g.us"), None);
    }
}
