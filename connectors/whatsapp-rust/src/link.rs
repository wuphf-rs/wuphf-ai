//! Setup's account actions: Link account (pair on the setup screen by the
//! configured login method, then fetch the chats into the chat directory)
//! and Refresh chats (fetch them again on the linked session). The Go
//! connectors' setup does the same (`connectors/shared/setup`).
//!
//! The bot runs on the connector's tokio runtime, while the screen's session
//! lives on setup's worker thread: the bot's events reach it over a channel.

use std::path::Path;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Context as _;
use botconf_tui::Session;
use tokio::runtime::Handle;
use whatsapp_rust::client::Client;
use whatsapp_rust::pair_code::PairCodeOptions;
use whatsapp_rust::prelude::*;
use whatsapp_rust::types::events::Event;
use whatsapp_rust::{GroupMetadata, Jid, Server};
use wuphf_connector::session_lock::{SessionLock, SessionLockError};
use wuphf_model::{
    CHAT_DIRECTORY_ENV, ChatAddress, ChatDirectory, ChatRef, ConnectorId, DirectoryChat,
    DirectoryChatKind, chat_directory,
};

use crate::chats::contact_forms;
use crate::community;
use crate::config::{Config, LoginMethod};
use crate::contacts::{self, Contacts};
use crate::device;
use crate::identity::Identity;
use crate::normalize::bare_id;
use crate::posting;

/// How long a linked session gets to connect.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// After a new link: how long to wait for the first contact, for a pause in
/// them (the sync is done), and at most.
const FIRST_CONTACT: Duration = Duration::from_secs(45);
const CONTACTS_QUIET: Duration = Duration::from_secs(10);
const CONTACTS_TIMEOUT: Duration = Duration::from_secs(90);
/// A pair code lasts about 160 s; a new one is asked for before then.
const PAIR_CODE_EVERY: Duration = Duration::from_secs(150);

/// Which action runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Link,
    Refresh,
}

/// What the bot tells the action.
enum Step {
    Qr { code: String, valid: Duration },
    PairCode(String),
    PairCodeFailed(String),
    Paired,
    Connected,
    Contact,
}

/// Run an action on the saved config, giving its notice or why it failed.
pub fn run(
    config: &Config,
    runtime: &Handle,
    screen: &mut dyn Session,
    mode: Mode,
) -> Result<String, String> {
    let session = Path::new(&config.session_database);
    let _lock = SessionLock::acquire(session).map_err(|error| match error {
        SessionLockError::Held(_) => format!(
            "wuphf-whatsapp-rust is running on {}: stop it, then try again",
            session.display()
        ),
        other => other.to_string(),
    })?;
    let directory = chat_directory().ok_or_else(|| {
        format!("no chat directory: set {CHAT_DIRECTORY_ENV} (or $XDG_STATE_HOME or $HOME)")
    })?;
    let (steps, events) = mpsc::channel();
    let (handle, contacts) = runtime
        .block_on(start(config, steps))
        .map_err(|error| format!("{error:#}"))?;
    let client = handle.client();
    let result = drive(config, runtime, screen, mode, &events, &client, &contacts).and_then(
        |(linked, chats)| {
            let count = chats.len();
            let path = ChatDirectory::new(
                ConnectorId(config.connector_id.clone()),
                "whatsapp",
                unix_ms(),
                chats,
            )
            .write(&directory)
            .map_err(|error| error.to_string())?;
            let saved = format!("{count} chats saved to {}", path.display());
            Ok(match linked {
                Some(linked) => format!("{linked}; {saved}"),
                None => saved,
            })
        },
    );
    runtime.block_on(handle.shutdown());
    result
}

/// Follow the bot until it is connected (pairing first when the session has
/// no device and the action links), wait for a new link's contacts, and
/// fetch the chats; the "linked as" notice when the action links.
fn drive(
    config: &Config,
    runtime: &Handle,
    screen: &mut dyn Session,
    mode: Mode,
    events: &mpsc::Receiver<Step>,
    client: &Arc<Client>,
    contacts: &Contacts,
) -> Result<(Option<String>, Vec<DirectoryChat>), String> {
    let started = Instant::now();
    let (mut pairing, mut paired, mut contacts_seen) = (false, false, 0usize);
    loop {
        if screen.cancelled() {
            return Err("Stopped.".into());
        }
        match events.recv_timeout(Duration::from_millis(100)) {
            Ok(Step::Qr { code, valid }) => {
                if mode == Mode::Refresh {
                    return Err("not linked yet: run Link account first".into());
                }
                if !pairing {
                    pairing = true;
                    screen.say(&match config.pair_code_phone() {
                        Some(phone) => format!(
                            "Linking by pair code for +{phone}: WhatsApp on the phone asks for it."
                        ),
                        None => "Scan the QR code with the phone's WhatsApp.".into(),
                    });
                }
                if config.login_method() == LoginMethod::Qr {
                    screen.qr(&code);
                    screen.progress(&format!(
                        "WhatsApp → Settings → Linked devices → Link a device. This code is valid \
                         for {} s; a new one follows.",
                        valid.as_secs()
                    ));
                }
            }
            Ok(Step::PairCode(code)) => screen.say(&format!(
                "Pair code {code}: WhatsApp → Settings → Linked devices → Link a device → Link \
                 with phone number instead. Valid for about 160 s; a new one follows."
            )),
            Ok(Step::PairCodeFailed(error)) => screen.say(&format!(
                "WhatsApp gave no pair code ({error}); the next round asks again."
            )),
            Ok(Step::Paired) => {
                paired = true;
                screen.say("Linked.");
            }
            Ok(Step::Connected) => break,
            Ok(Step::Contact) => contacts_seen += 1,
            Err(RecvTimeoutError::Timeout) => {
                // Pairing waits for the phone as long as it takes; a linked
                // session connects soon.
                if !pairing && started.elapsed() > CONNECT_TIMEOUT {
                    return Err("WhatsApp did not connect in 30 s".into());
                }
            }
            Err(RecvTimeoutError::Disconnected) => return Err("the WhatsApp client stopped".into()),
        }
    }
    let linked = (mode == Mode::Link).then(|| {
        if !paired {
            screen.say("This session is linked already.");
        }
        match client.get_pn() {
            Some(pn) => format!("Linked as +{}", pn.user),
            None => "Linked".to_owned(),
        }
    });
    if paired {
        wait_for_contacts(screen, events, contacts_seen)?;
    }
    screen.progress("Fetching the groups, contacts and channels…");
    let (chats, channels_failed) = runtime
        .block_on(fetch(config, client, contacts))
        .map_err(|error| format!("{error:#}"))?;
    if let Some(error) = channels_failed {
        screen.say(&format!("The channels could not be fetched: {error}"));
    }
    Ok((linked, chats))
}

/// After a new link WhatsApp sends the contacts in its first app-state sync,
/// which has no end event: wait for the first, then for a pause, at most
/// CONTACTS_TIMEOUT.
fn wait_for_contacts(
    screen: &mut dyn Session,
    events: &mpsc::Receiver<Step>,
    mut seen: usize,
) -> Result<(), String> {
    let started = Instant::now();
    let mut last = started;
    loop {
        if screen.cancelled() {
            return Err("Stopped.".into());
        }
        let quiet = if seen == 0 {
            FIRST_CONTACT
        } else {
            CONTACTS_QUIET
        };
        if last.elapsed() >= quiet || started.elapsed() >= CONTACTS_TIMEOUT {
            if seen == 0 {
                screen.say(
                    "WhatsApp has not sent the contacts yet: the groups are saved, and Refresh \
                     chats later adds the contacts.",
                );
            }
            return Ok(());
        }
        screen.progress(&format!(
            "Waiting for WhatsApp to send the contacts… {seen} so far ({} s)",
            started.elapsed().as_secs()
        ));
        match events.recv_timeout(Duration::from_millis(500)) {
            Ok(Step::Contact) => {
                seen += 1;
                last = Instant::now();
            }
            Ok(_) | Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return Err("the WhatsApp client stopped".into()),
        }
    }
}

/// Open the session store and start a bot on it whose events go to `steps`:
/// pairing codes, the link, the connection, and the contacts a sync stores
/// (in the connector's contacts table, as the connector would).
async fn start(
    config: &Config,
    steps: mpsc::Sender<Step>,
) -> anyhow::Result<(BotHandle, Arc<Contacts>)> {
    let store = SqliteStore::new(&config.session_database)
        .await
        .context("open the WhatsApp session store")?;
    let contacts = Arc::new(
        Contacts::open(store.shared())
            .await
            .context("prepare the WhatsApp contacts table")?,
    );
    let (profile, device_props) = device::client_identity(config.client_mode);
    let phone = config.pair_code_phone().map(str::to_owned);
    let last_ask = Arc::new(Mutex::new(None::<Instant>));
    let (qr_steps, paired_steps, connected_steps, contact_steps) =
        (steps.clone(), steps.clone(), steps.clone(), steps);
    let stored = Arc::clone(&contacts);
    let bot = Bot::builder()
        .with_backend(store)
        .with_device_props(device_props)
        .on_event_for(
            &[EventKind::PairingQrCode, EventKind::PairingCodeRefresh],
            move |event, client| {
                let (steps, phone, last_ask) =
                    (qr_steps.clone(), phone.clone(), Arc::clone(&last_ask));
                async move {
                    let refresh = match &*event {
                        Event::PairingQrCode(qr) => {
                            let _ = steps.send(Step::Qr {
                                code: qr.code.clone(),
                                valid: qr.timeout,
                            });
                            false
                        }
                        Event::PairingCodeRefresh(_) => true,
                        _ => return,
                    };
                    let Some(phone) = phone else {
                        return;
                    };
                    if !pair_code_due(&last_ask, refresh, Instant::now()) {
                        return;
                    }
                    // Off the event path: asking waits on WhatsApp.
                    tokio::spawn(async move {
                        let options = PairCodeOptions {
                            show_push_notification: true,
                            ..PairCodeOptions::for_phone(phone)
                        };
                        let _ = steps.send(match client.pair_with_code(options).await {
                            Ok(code) => Step::PairCode(code),
                            Err(error) => Step::PairCodeFailed(error.to_string()),
                        });
                    });
                }
            },
        )
        .on_event_for(&[EventKind::PairSuccess], move |_event, _client| {
            let steps = paired_steps.clone();
            async move {
                let _ = steps.send(Step::Paired);
            }
        })
        .on_connected(move |_client| {
            let _ = connected_steps.send(Step::Connected);
            async {}
        })
        .on_event_for(&[EventKind::ContactUpdate], move |event, _client| {
            let (steps, contacts) = (contact_steps.clone(), Arc::clone(&stored));
            async move {
                let Event::ContactUpdate(update) = &*event else {
                    return;
                };
                let jid = update.jid.to_non_ad();
                if !matches!(jid.server, Server::Pn | Server::Lid) {
                    return;
                }
                contacts
                    .upsert_contact_name(
                        contact_forms(&jid, &update.action),
                        update.action.full_name.as_deref(),
                        update.action.first_name.as_deref(),
                    )
                    .await;
                let _ = steps.send(Step::Contact);
            }
        })
        .build()
        .await
        .context("build the WhatsApp bot")?;
    // As the connector does: before the first connect.
    bot.client().set_client_profile(profile).await;
    Ok((bot.spawn(), contacts))
}

/// Whether to ask for a pair code now: when WhatsApp asks for a new one, the
/// first time, and before the last one runs out.
fn pair_code_due(last: &Mutex<Option<Instant>>, refresh: bool, now: Instant) -> bool {
    let mut last = last.lock().unwrap();
    let due = refresh || last.is_none_or(|at| now.duration_since(at) >= PAIR_CODE_EVERY);
    if due {
        *last = Some(now);
    }
    due
}

/// The joined groups as the directory lists them (plan 085): a community as
/// one, a group in a community as "Community › Group".
fn directory_groups<'a>(
    groups: impl Iterator<Item = &'a GroupMetadata> + Clone,
) -> Vec<(String, String, DirectoryChatKind)> {
    let names = posting::group_names(groups.clone());
    groups
        .map(|group| {
            let community = community::parent_of(group)
                .and_then(|parent| names.get(&bare_id(parent)))
                .map(String::as_str);
            let kind = if group.is_parent_group {
                DirectoryChatKind::Community
            } else {
                DirectoryChatKind::Group
            };
            (
                bare_id(&group.id),
                community::community_name(community, &group.subject),
                kind,
            )
        })
        .collect()
}

/// The joined groups, the followed channels and the saved contacts, with the
/// ids the connector reports to the core; and why the channels could not be
/// listed, when they could not (the rest is still worth saving).
async fn fetch(
    config: &Config,
    client: &Arc<Client>,
    contacts: &Contacts,
) -> anyhow::Result<(Vec<DirectoryChat>, Option<String>)> {
    let groups = client
        .groups()
        .get_participating()
        .await
        .context("fetch the groups")?;
    let mut chats = directory_groups(groups.values());
    let channels_failed = match client.newsletter().list_subscribed().await {
        Ok(channels) => {
            chats.extend(channels.into_iter().map(|channel| {
                (
                    bare_id(&channel.jid),
                    channel.name,
                    DirectoryChatKind::Channel,
                )
            }));
            None
        }
        Err(error) => Some(error.to_string()),
    };
    let saved = contacts
        .run(contacts::saved)
        .await
        .context("read the contacts")?;
    let identity = Identity::default();
    for contact in saved {
        if let Ok(jid) = contact.jid.parse::<Jid>() {
            chats.push((
                identity.canonical_id(client.as_ref(), &jid).await,
                contact.full_name,
                DirectoryChatKind::Direct,
            ));
        }
    }
    Ok((
        directory_chats(&config.connector_id, chats),
        channels_failed,
    ))
}

/// The directory's chats from (id, title, kind), each id once (a contact
/// saved under its phone and its LID form is one chat), by title.
fn directory_chats(
    connector_id: &str,
    chats: Vec<(String, String, DirectoryChatKind)>,
) -> Vec<DirectoryChat> {
    let mut out: Vec<DirectoryChat> = Vec::new();
    for (id, title, kind) in chats {
        let chat = ChatRef {
            connector_id: ConnectorId(connector_id.to_owned()),
            address: ChatAddress::new(id.clone(), None),
        };
        if out.iter().any(|seen| seen.chat == chat) {
            continue;
        }
        let title = match title.trim() {
            "" => id,
            title => title.to_owned(),
        };
        out.push(DirectoryChat { chat, title, kind });
    }
    out.sort_by(|a, b| {
        (a.title.to_lowercase(), a.chat.to_string())
            .cmp(&(b.title.to_lowercase(), b.chat.to_string()))
    });
    out
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_directory_holds_groups_channels_and_saved_contacts_once_each() {
        let (group, channel, direct) = (
            DirectoryChatKind::Group,
            DirectoryChatKind::Channel,
            DirectoryChatKind::Direct,
        );
        let chats = directory_chats(
            "whatsapp-main",
            vec![
                ("1@g.us".into(), "Family".into(), group),
                ("2@g.us".into(), " ".into(), group),
                ("5@newsletter".into(), "Club news".into(), channel),
                ("9@lid".into(), "Alice".into(), direct),
                // Her phone form canonicalized to the same LID.
                ("9@lid".into(), "Alice".into(), direct),
            ],
        );
        let read: Vec<_> = chats
            .iter()
            .map(|chat| (chat.chat.to_string(), chat.title.as_str(), chat.kind))
            .collect();
        assert_eq!(
            read,
            [
                (
                    "whatsapp-main:2@g.us".to_owned(),
                    "2@g.us",
                    DirectoryChatKind::Group
                ),
                (
                    "whatsapp-main:9@lid".to_owned(),
                    "Alice",
                    DirectoryChatKind::Direct
                ),
                (
                    "whatsapp-main:5@newsletter".to_owned(),
                    "Club news",
                    DirectoryChatKind::Channel
                ),
                (
                    "whatsapp-main:1@g.us".to_owned(),
                    "Family",
                    DirectoryChatKind::Group
                ),
            ]
        );
    }

    #[test]
    fn a_pair_code_is_asked_for_first_on_refresh_and_before_it_runs_out() {
        let last = Mutex::new(None);
        let start = Instant::now();
        assert!(pair_code_due(&last, false, start), "the first");
        assert!(
            !pair_code_due(&last, false, start + Duration::from_secs(20)),
            "a QR rotation"
        );
        assert!(
            pair_code_due(&last, true, start + Duration::from_secs(30)),
            "WhatsApp asks"
        );
        assert!(
            pair_code_due(&last, false, start + Duration::from_secs(181)),
            "running out"
        );
    }

    #[test]
    fn the_directory_lists_communities_and_titles_their_groups_under_them() {
        let group =
            |jid: &str, subject: &str, parent: Option<&str>, is_parent: bool| GroupMetadata {
                id: jid.parse().unwrap(),
                subject: subject.into(),
                parent_group_jid: parent.map(|jid| jid.parse().unwrap()),
                is_parent_group: is_parent,
                ..GroupMetadata::default()
            };
        let groups = [
            group("9@g.us", "Neighbours", None, true),
            group("1@g.us", "Garden", Some("9@g.us"), false),
            group("2@g.us", "Book club", None, false),
        ];
        assert_eq!(
            directory_groups(groups.iter()),
            [
                (
                    "9@g.us".into(),
                    "Neighbours".into(),
                    DirectoryChatKind::Community
                ),
                (
                    "1@g.us".into(),
                    "Neighbours › Garden".into(),
                    DirectoryChatKind::Group
                ),
                (
                    "2@g.us".into(),
                    "Book club".into(),
                    DirectoryChatKind::Group
                ),
            ]
        );
    }
}
