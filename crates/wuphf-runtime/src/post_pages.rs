//! The panel's post pages (plan 084, on botconf's data pages): a pending
//! post's preview with ✅ Post / ✖️ Cancel, and "📢 Post a status" for a topic
//! whose source takes posts. Both open by where the panel was opened: the
//! core asks the topic's connector to open the panel there after a `/status`.
//!
//! Routes: `post/<id>` (a pending post), `compose` (the topic the panel was
//! opened in). [`DataPages`] puts them next to the Health page, which keeps
//! the root.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use botconf_panel::{
    ActOutcome, Context, DataAction, DataActionKind, DataInput, DataPage, DataSource, DataViewer,
    InputKind, Text,
};
use std::str::FromStr;
use uuid::Uuid;
use wuphf_core::{PENDING_POST_TTL_MS, PendingPost, PostOutcome};
use wuphf_model::{AttachmentKind, ChatAddress, ChatRef, ConnectorId, MessageId};
use wuphf_storage::Storage;

use crate::OwnerMessageSender;
use crate::owner_send::now_ms;

const POST: &str = "post/";
/// `compose/<connector:chat#thread>`: the topic rides in the route, since only
/// the command that opens the panel says where it was opened.
const COMPOSE: &str = "compose/";

const CONFIRM: &str = "post";
const CANCEL: &str = "cancel";
const WRITE: &str = "write";

type Page<'a, T> = Pin<Box<dyn Future<Output = Result<T, String>> + Send + 'a>>;

/// Whether `route` is one of the post pages'.
pub fn is_post_route(route: &str) -> bool {
    route.starts_with(POST) || route.starts_with(COMPOSE)
}

/// The post pages, posting through the owner sender (so what they post is
/// delivered like any owner message).
pub struct PostPages<S> {
    sender: OwnerMessageSender<S>,
}

impl<S> PostPages<S> {
    pub fn new(sender: OwnerMessageSender<S>) -> Self {
        Self { sender }
    }
}

/// The topic the panel was opened in, on the viewer's connector: the actor is
/// `<connector>:<user>` (see `settings_auth`).
fn topic(viewer: &DataViewer) -> Option<(ConnectorId, ChatAddress)> {
    let context = viewer.context.as_ref()?;
    let connector = viewer.actor.as_deref()?.split_once(':')?.0;
    Some((ConnectorId(connector.to_owned()), address(context)))
}

fn address(context: &Context) -> ChatAddress {
    ChatAddress::new(context.chat.clone(), context.thread.clone())
}

/// The compose route of a topic.
fn compose_route(connector: ConnectorId, address: ChatAddress) -> String {
    format!(
        "{COMPOSE}{}",
        ChatRef {
            connector_id: connector,
            address,
        }
    )
}

/// The topic a compose route names.
fn composed_topic(route: &str) -> Option<(ConnectorId, ChatAddress)> {
    let chat = ChatRef::from_str(route.strip_prefix(COMPOSE)?).ok()?;
    Some((chat.connector_id, chat.address))
}

fn post_id(route: &str) -> Option<MessageId> {
    Uuid::parse_str(route.strip_prefix(POST)?)
        .ok()
        .map(MessageId)
}

impl<S: Storage> DataSource for PostPages<S> {
    fn page<'a>(&'a self, _viewer: &'a DataViewer, route: &'a str) -> Page<'a, DataPage> {
        Box::pin(async move {
            if route.starts_with(COMPOSE) {
                return self.compose(composed_topic(route)).await;
            }
            let id = post_id(route).ok_or("no such page")?;
            self.preview(id).await
        })
    }

    fn act<'a>(
        &'a self,
        _viewer: &'a DataViewer,
        route: &'a str,
        action: &'a str,
        input: Option<DataInput>,
    ) -> Page<'a, ActOutcome> {
        Box::pin(async move {
            let outcome = match (action, post_id(route)) {
                (CONFIRM, Some(id)) => self.sender.confirm_post(id).await,
                (CANCEL, Some(id)) => {
                    let cancelled = self.sender.core().cancel_post(id);
                    return Ok(ActOutcome::Message(Text::plain(if cancelled {
                        "✖️ Not posted."
                    } else {
                        "This post is gone already."
                    })));
                }
                (WRITE, None) if route.starts_with(COMPOSE) => {
                    let Some(DataInput::Text(text)) = input else {
                        return Err("type the status".into());
                    };
                    let (connector, address) = composed_topic(route).ok_or("no such topic")?;
                    self.sender.compose_post(&connector, &address, &text).await
                }
                _ => return Err("this action is not offered here".into()),
            }
            .map_err(|error| error.to_string())?;
            Ok(ActOutcome::Message(Text::plain(match outcome {
                PostOutcome::Posted(_) => {
                    "📢 Posted. It shows 👍 in the topic once it is up.".into()
                }
                PostOutcome::Pending(_) => "Waiting for your ✅.".into(),
                PostOutcome::Refused(reason) => reason,
            })))
        })
    }

    fn open<'a>(
        &'a self,
        viewer: &'a DataViewer,
        _context: &'a Context,
    ) -> Pin<Box<dyn Future<Output = Option<String>> + Send + 'a>> {
        Box::pin(async move {
            let (connector, address) = topic(viewer)?;
            let core = self.sender.core();
            if let Ok(Some(pending)) = core.pending_post_at(&connector, &address, now_ms()).await {
                return Some(format!("{POST}{}", pending.id));
            }
            match core.postable_topic(&connector, &address).await {
                Ok(Some(_)) => Some(compose_route(connector, address)),
                _ => None,
            }
        })
    }
}

impl<S: Storage> PostPages<S> {
    async fn preview(&self, id: MessageId) -> Result<DataPage, String> {
        let now = now_ms();
        let core = self.sender.core();
        let Some(pending) = core.pending_post(id, now) else {
            return Ok(DataPage {
                title: "📢 Post".into(),
                body: Text::plain("This post expired or was handled; send /status again."),
                ..DataPage::default()
            });
        };
        let message = core
            .storage()
            .message(pending.message_id)
            .await
            .map_err(|error| error.to_string())?;
        let mut body = Text::new();
        body.push_plain(format!("Post to {}?\n\n", pending.title));
        let text = pending
            .caption
            .clone()
            .or_else(|| message.as_ref().and_then(|message| message.text.clone()));
        let kinds: Vec<&str> = message
            .iter()
            .flat_map(|message| &message.attachments)
            .map(|attachment| kind(&attachment.kind))
            .collect();
        if !kinds.is_empty() {
            body.push_plain(format!("{}\n", kinds.join(", ")));
        }
        match text.as_deref().filter(|text| !text.is_empty()) {
            Some(text) => {
                body.push_plain(format!("{text}\n"));
            }
            None if kinds.is_empty() => {
                body.push_plain("(nothing)\n");
            }
            None => {}
        }
        body.push_plain(format!("\n{}", footer(&pending, now)));
        Ok(DataPage {
            title: "📢 Post".into(),
            body,
            actions: vec![
                DataAction {
                    id: CONFIRM.into(),
                    label: "✅ Post".into(),
                    kind: DataActionKind::Run,
                },
                DataAction {
                    id: CANCEL.into(),
                    label: "✖️ Cancel".into(),
                    kind: DataActionKind::Run,
                },
            ],
            ..DataPage::default()
        })
    }

    async fn compose(&self, topic: Option<(ConnectorId, ChatAddress)>) -> Result<DataPage, String> {
        let Some((connector, address)) = topic else {
            return Ok(DataPage {
                title: "📢 Post".into(),
                body: Text::plain("Open the panel in a Status topic to post a status."),
                ..DataPage::default()
            });
        };
        let postable = self
            .sender
            .core()
            .postable_topic(&connector, &address)
            .await
            .map_err(|error| error.to_string())?;
        let Some((_, title, audience)) = postable else {
            return Ok(DataPage {
                title: "📢 Post".into(),
                body: Text::plain("This topic's chat takes no posts."),
                ..DataPage::default()
            });
        };
        let mut body = Text::new();
        body.push_plain(format!("Posts go to {title}"));
        if let Some(audience) = audience {
            body.push_plain(format!(" (who sees them: {audience})"));
        }
        body.push_plain(
            ".\n\nOr reply /status to a message in the topic (/status! posts it at once).",
        );
        Ok(DataPage {
            title: "📢 Post".into(),
            body,
            actions: vec![DataAction {
                id: WRITE.into(),
                label: "📢 Post a status".into(),
                kind: DataActionKind::Ask {
                    prompt: "Type the status. It is posted as soon as you send it.".into(),
                    input: InputKind::Text,
                },
            }],
            ..DataPage::default()
        })
    }
}

/// Who sees the post, and how long the preview waits.
fn footer(pending: &PendingPost, now_ms: i64) -> String {
    let left = (pending.created_ms + PENDING_POST_TTL_MS - now_ms).max(0) / 60_000;
    let audience = pending
        .audience
        .as_deref()
        .unwrap_or("your status privacy setting");
    format!("Who sees it: {audience}.\nThis preview expires in {left} min.")
}

fn kind(kind: &AttachmentKind) -> &'static str {
    match kind {
        AttachmentKind::Image => "🖼 photo",
        AttachmentKind::Video => "🎬 video",
        AttachmentKind::Audio => "🎤 audio",
        AttachmentKind::Sticker => "sticker",
        AttachmentKind::File => "📎 file",
    }
}

/// The core's data pages: the Health page at the root, and the post pages,
/// which open by where the panel was opened.
pub struct DataPages {
    health: Arc<dyn DataSource>,
    posts: Arc<dyn DataSource>,
}

impl DataPages {
    pub fn new(health: Arc<dyn DataSource>, posts: Arc<dyn DataSource>) -> Self {
        Self { health, posts }
    }

    /// The pages as the settings server takes a data source.
    pub fn shared(health: Arc<dyn DataSource>, posts: Arc<dyn DataSource>) -> Arc<dyn DataSource> {
        Arc::new(Self::new(health, posts))
    }

    fn source(&self, route: &str) -> &Arc<dyn DataSource> {
        if is_post_route(route) {
            &self.posts
        } else {
            &self.health
        }
    }
}

impl DataSource for DataPages {
    fn page<'a>(&'a self, viewer: &'a DataViewer, route: &'a str) -> Page<'a, DataPage> {
        self.source(route).page(viewer, route)
    }

    fn act<'a>(
        &'a self,
        viewer: &'a DataViewer,
        route: &'a str,
        action: &'a str,
        input: Option<DataInput>,
    ) -> Page<'a, ActOutcome> {
        self.source(route).act(viewer, route, action, input)
    }

    fn open<'a>(
        &'a self,
        viewer: &'a DataViewer,
        context: &'a Context,
    ) -> Pin<Box<dyn Future<Output = Option<String>> + Send + 'a>> {
        self.posts.open(viewer, context)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Runtime;
    use std::collections::{HashMap, HashSet};
    use wuphf_core::{Core, RoutingConfig};
    use wuphf_model::{
        Action, ChatFilter, ChatPosting, ChatUpdate, Connector, ConnectorCapabilities, DispatchAck,
        Event, EventKind, NewMessage, Platform, Rule, RuleId, RuleKind, SourceAddress,
    };
    use wuphf_storage::InMemoryStorage;

    const WA: &str = "whatsapp-main";
    const TG: &str = "telegram-main";
    const FORUM: &str = "telegram:channel:7";
    const STATUS: &str = "status@broadcast";
    const OWNER: &str = "704";

    fn core() -> Arc<Core<InMemoryStorage>> {
        let rule = Rule {
            id: RuleId("wa-all".into()),
            flow: RuleKind::ManyToOne {
                source_connector: ConnectorId(WA.into()),
                source_chats_filter: ChatFilter::All,
                destination_connector: ConnectorId(TG.into()),
                destination_chat: ChatAddress::new(FORUM, None),
            },
        };
        let connectors = vec![
            Connector {
                id: ConnectorId(WA.into()),
                platform: Platform::WhatsApp,
                capabilities: ConnectorCapabilities::default(),
            },
            Connector {
                id: ConnectorId(TG.into()),
                platform: Platform::Telegram,
                capabilities: ConnectorCapabilities {
                    supports_threads: true,
                    supports_topic_create: true,
                    supports_disappearing_timers: false,
                },
            },
        ];
        let owners = HashMap::from([(ConnectorId(TG.into()), HashSet::from([OWNER.to_owned()]))]);
        let routing = RoutingConfig::new(vec![rule], connectors).with_owners(owners);
        Arc::new(Core::new(
            Arc::new(InMemoryStorage::default()),
            Arc::new(routing),
        ))
    }

    fn message(connector: &str, chat: &str, thread: Option<&str>, id: &str, text: &str) -> Event {
        Event {
            source: SourceAddress {
                connector_id: ConnectorId(connector.into()),
                address: ChatAddress::new(chat, thread.map(str::to_owned)),
                chat_title: Some("Status".into()),
            },
            payload: EventKind::NewMessage(NewMessage {
                native_id: id.into(),
                author_id: "someone".into(),
                participant: None,
                author_name: None,
                text: Some(text.into()),
                attachments: vec![],
                reply_to_native_id: None,
                timestamp_ms: now_ms(),
                disappearing: None,
                author_account: (connector == TG).then(|| OWNER.to_owned()),
                reply_to_chat_id: None,
            }),
        }
    }

    /// A core whose Status feed takes posts and is mirrored into topic 100.
    async fn status_feed() -> Arc<Core<InMemoryStorage>> {
        let core = core();
        core.update_chats(
            &ConnectorId(WA.into()),
            vec![ChatUpdate {
                posting: Some(ChatPosting {
                    messages: false,
                    reactions: true,
                    private_replies: true,
                    posts: true,
                    audience: Some("My contacts".into()),
                }),
                ..ChatUpdate::new(STATUS)
            }],
            true,
        )
        .await
        .unwrap();
        let outcome = core
            .ingest(message(WA, STATUS, None, "wa-1", "a story"))
            .await
            .unwrap();
        let card = &outcome.dispatches()[0];
        core.acknowledge_dispatch(DispatchAck {
            dispatch_id: card.id,
            native_id: "tg-card".into(),
            thread_id: Some("100".into()),
        })
        .await
        .unwrap();
        core
    }

    fn compose() -> String {
        format!("{COMPOSE}{TG}:{FORUM}#100")
    }

    /// A press or an answer: the renderer sends no context then.
    fn presser() -> DataViewer {
        DataViewer {
            actor: Some(format!("{TG}:{OWNER}")),
            context: None,
        }
    }

    fn viewer() -> DataViewer {
        DataViewer {
            actor: Some(format!("{TG}:{OWNER}")),
            context: Some(Context {
                chat: FORUM.into(),
                thread: Some("100".into()),
            }),
        }
    }

    #[tokio::test]
    async fn the_panel_opens_on_a_pending_post_and_posts_it() {
        let core = status_feed().await;
        let (runtime, mut channels, _observer) = Runtime::channelled(Arc::clone(&core), 8);
        let pages = PostPages::new(runtime.owner_sender());
        let _task = tokio::spawn(runtime.run());

        // In the topic, before any /status: the compose page.
        let context = viewer().context.unwrap();
        assert_eq!(pages.open(&viewer(), &context).await, Some(compose()));

        core.ingest(message(TG, FORUM, Some("100"), "tg-cmd", "/status hello"))
            .await
            .unwrap();
        let route = pages
            .open(&viewer(), &context)
            .await
            .expect("a pending post");
        assert!(route.starts_with(POST), "{route}");
        let preview = pages.page(&viewer(), &route).await.unwrap();
        let body = format!("{:?}", preview.body);
        assert!(
            body.contains("hello") && body.contains("My contacts"),
            "{body}"
        );
        assert_eq!(preview.actions.len(), 2);

        pages.act(&presser(), &route, CONFIRM, None).await.unwrap();
        let posted = channels.dispatches.recv().await.unwrap();
        assert_eq!(posted.to.address.chat_id, STATUS);
        assert!(
            matches!(&posted.action, Action::Post { message } if message.text.as_deref() == Some("hello"))
        );
        // Handled: the panel opens on the compose page again.
        assert_eq!(pages.open(&viewer(), &context).await, Some(compose()));
    }

    #[tokio::test]
    async fn a_status_typed_in_the_panel_is_posted() {
        let core = status_feed().await;
        let (runtime, mut channels, _observer) = Runtime::channelled(Arc::clone(&core), 8);
        let pages = PostPages::new(runtime.owner_sender());
        let _task = tokio::spawn(runtime.run());
        pages
            .act(
                &presser(),
                &compose(),
                WRITE,
                Some(DataInput::Text("typed".into())),
            )
            .await
            .unwrap();
        let mut posted = false;
        for _ in 0..2 {
            let dispatch = channels.dispatches.recv().await.unwrap();
            posted |= matches!(&dispatch.action, Action::Post { .. });
        }
        assert!(posted);
    }

    #[tokio::test]
    async fn a_topic_that_takes_no_posts_opens_nothing() {
        let core = core();
        let (runtime, _channels, _observer) = Runtime::channelled(core, 8);
        let pages = PostPages::new(runtime.owner_sender());
        let context = viewer().context.unwrap();
        assert_eq!(pages.open(&viewer(), &context).await, None);
        let page = pages.page(&presser(), &compose()).await.unwrap();
        assert!(page.actions.is_empty());
    }
}
