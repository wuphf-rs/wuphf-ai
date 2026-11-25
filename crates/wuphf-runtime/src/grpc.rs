use crate::auth::{ConnectorTokens, bearer_token};
use crate::connector::{ConnectorConnection, ConnectorHubHandle, RegistrationError};
use crate::connector_settings::{ConnectorSettings, declaration_from_wire};
use crate::live_sessions::LiveSessions;
use std::pin::Pin;
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};
use tokio_stream::{Stream, wrappers::ReceiverStream};
use tonic::{Request, Response, Status};
use uuid::Uuid;
use wuphf_model::{
    Action, Attachment, AttachmentKind, ChatAddress, ChatPosting, ChatUpdate, Connector,
    ConnectorCapabilities, ConnectorId, ConnectorInput, ConnectorOutput, DisappearingTimer,
    DispatchAck, DispatchFailed, Event, EventKind, InboundWork, MediaId, MuteState, NewMessage,
    NoticeKind, ParticipantChange, ParticipantChangeKind, Platform, ReactorReaction, ReceiptKind,
    ReplyContext, ReportedAvatar, SourceAddress,
};
use wuphf_proto::connector as pb;
use wuphf_proto::connector::connector_transport_server::ConnectorTransport;

pub struct GrpcConnectorService {
    hub: ConnectorHubHandle,
    tokens: Arc<ConnectorTokens>,
    /// Where connectors' settings declarations go; `None` refuses them.
    settings: Option<Arc<ConnectorSettings>>,
    /// Where live sessions' commands are offered and run; `None` ignores the
    /// commands connectors declare.
    commands: Option<Arc<LiveSessions>>,
}

impl GrpcConnectorService {
    /// A session is accepted only when it presents the token configured for
    /// the connector id in its registration frame.
    pub fn new(hub: ConnectorHubHandle, tokens: Arc<ConnectorTokens>) -> Self {
        Self {
            hub,
            tokens,
            settings: None,
            commands: None,
        }
    }

    /// Offer the commands live sessions declare, and run them on their
    /// sessions.
    pub fn with_commands(mut self, commands: Arc<LiveSessions>) -> Self {
        self.commands = Some(commands);
        self
    }

    /// Accept connectors' settings declarations, each registered as a section
    /// of the core's settings.
    pub fn with_settings(mut self, settings: Arc<ConnectorSettings>) -> Self {
        self.settings = Some(settings);
        self
    }
}

#[tonic::async_trait]
impl ConnectorTransport for GrpcConnectorService {
    type OpenSessionStream =
        Pin<Box<dyn Stream<Item = Result<pb::ServerFrame, Status>> + Send + 'static>>;

    async fn open_session(
        &self,
        request: Request<tonic::Streaming<pb::ClientFrame>>,
    ) -> Result<Response<Self::OpenSessionStream>, Status> {
        let token = bearer_token(request.metadata()).map(str::to_owned);
        let mut incoming = request.into_inner();
        let first = incoming
            .message()
            .await?
            .ok_or_else(|| Status::invalid_argument("registration frame required"))?;
        let (registration, declaration, commands, tools) = match first.body {
            Some(pb::client_frame::Body::Registration(mut registration)) => {
                // The declaration is not part of the connector's identity: the
                // hub matches the rest against the configured entry.
                let declaration = registration.settings.take();
                let commands = std::mem::take(&mut registration.commands);
                let tools = std::mem::take(&mut registration.tools);
                let registration = registration_from_proto(registration)
                    .map_err(|error| Status::invalid_argument(error.to_string()))?;
                (registration, declaration, commands, tools)
            }
            _ => return Err(Status::invalid_argument("first frame must be registration")),
        };
        if !self
            .tokens
            .authenticates(&registration.id, token.as_deref())
        {
            tracing::warn!(
                connector = %registration.id.0,
                "rejecting connector session: unknown connector or wrong token",
            );
            return Err(Status::unauthenticated("unknown connector or wrong token"));
        }
        // A declaration is checked before the session is accepted, so a
        // connector with bad settings never goes live; it is stored and
        // registered once the session is. A connector that declares nothing
        // keeps the section it declared before.
        let declaration = match declaration {
            None => None,
            Some(declaration) => {
                let Some(settings) = &self.settings else {
                    return Err(Status::invalid_argument(
                        "this core does not accept connector settings declarations",
                    ));
                };
                let declaration = declaration_from_wire(&registration.id, declaration);
                settings.section(&declaration).map_err(|error| {
                    Status::invalid_argument(format!("invalid settings declaration: {error}"))
                })?;
                Some((Arc::clone(settings), declaration))
            }
        };
        let connector_id = registration.id.clone();
        let connection = self
            .hub
            .connect(registration)
            .await
            .map_err(|error| match error {
                RegistrationError::Mismatch => Status::failed_precondition(error.to_string()),
                RegistrationError::AlreadyLive => Status::already_exists(error.to_string()),
                RegistrationError::HubStopped => Status::unavailable(error.to_string()),
            })?;
        if let Some((settings, declaration)) = declaration
            && let Err(error) = settings.declare(declaration).await
        {
            tracing::warn!(
                connector = %connector_id.0,
                %error,
                "could not register the connector's settings",
            );
        }
        let ConnectorConnection { input, mut output } = connection;
        // Lives as long as the outgoing task, so the session's commands are
        // offered exactly while it is live.
        let mut command_session = self
            .commands
            .as_ref()
            .map(|registry| registry.attach(&connector_id.0, commands, tools));
        let registry = self.commands.clone();
        // Fired when the reader returns for any reason, so the outgoing task
        // drops `output` at once and the hub sees the session as no longer live.
        let (ended_tx, mut ended_rx) = oneshot::channel::<()>();
        tokio::spawn(async move {
            let _ended = ended_tx;
            while let Ok(Some(frame)) = incoming.message().await {
                match &frame.body {
                    Some(pb::client_frame::Body::CommandResult(result)) => {
                        if let Some(registry) = &registry {
                            registry.settle(&connector_id.0, result.clone());
                        }
                        continue;
                    }
                    Some(pb::client_frame::Body::ProbeResult(result)) => {
                        if let Some(registry) = &registry {
                            registry.settle_probe(&connector_id.0, result.clone());
                        }
                        continue;
                    }
                    _ => {}
                }
                let message = match input_from_proto(frame, &connector_id) {
                    Ok(message) => message,
                    Err(error) => {
                        tracing::warn!(
                            connector = %connector_id.0,
                            %error,
                            "closing connector stream after malformed frame",
                        );
                        return;
                    }
                };
                if input.try_send(message).is_err() {
                    tracing::warn!(
                        connector = %connector_id.0,
                        "closing connector stream: input channel is full or closed",
                    );
                    return;
                }
            }
            tracing::info!(connector = %connector_id.0, "connector stream ended");
        });
        let (outgoing_tx, outgoing_rx) = mpsc::channel(32);
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    next = output.recv() => {
                        let Some(next) = next else { return };
                        if outgoing_tx.send(Ok(output_to_proto(next))).await.is_err() {
                            return;
                        }
                    }
                    Some(press) = next_press(&mut command_session) => {
                        if outgoing_tx.send(Ok(press)).await.is_err() {
                            return;
                        }
                    }
                    _ = &mut ended_rx => return,
                }
            }
        });
        Ok(Response::new(Box::pin(ReceiverStream::new(outgoing_rx))))
    }
}

/// The next press to write to the session; never, for a service without
/// commands.
async fn next_press(
    session: &mut Option<crate::live_sessions::LiveSession>,
) -> Option<pb::ServerFrame> {
    match session {
        Some(session) => session.frames.recv().await,
        None => std::future::pending().await,
    }
}

#[derive(Debug, Error)]
#[error("{0}")]
struct WireError(String);

fn registration_from_proto(registration: pb::Registration) -> Result<Connector, WireError> {
    Ok(Connector {
        id: ConnectorId(required_string(registration.connector_id, "connector_id")?),
        platform: platform_from_proto(registration.platform)?,
        capabilities: registration
            .capabilities
            .map(|capabilities| ConnectorCapabilities {
                supports_threads: capabilities.supports_threads,
                supports_topic_create: capabilities.supports_topic_create,
                supports_disappearing_timers: capabilities.supports_disappearing_timers,
            })
            .unwrap_or_default(),
    })
}

fn input_from_proto(
    frame: pb::ClientFrame,
    connector_id: &ConnectorId,
) -> Result<ConnectorInput, WireError> {
    match frame.body {
        Some(pb::client_frame::Body::MessageCreate(event)) => Ok(ConnectorInput::event(Event {
            source: SourceAddress {
                connector_id: connector_id.clone(),
                address: ChatAddress::new(
                    required_string(event.chat_id, "chat_id")?,
                    event.thread_id,
                ),
                chat_title: event.chat_title,
            },
            payload: EventKind::NewMessage(NewMessage {
                native_id: required_string(event.platform_message_id, "platform_message_id")?,
                author_id: required_string(event.sender, "sender")?,
                participant: event.participant,
                author_name: event.sender_name,
                text: event.text,
                attachments: event
                    .attachments
                    .into_iter()
                    .map(attachment_from_proto)
                    .collect::<Result<_, _>>()?,
                reply_to_native_id: event.reply_to_platform_message_id,
                timestamp_ms: event.timestamp_ms,
                disappearing: event.disappearing.map(timer_from_proto),
                author_account: event.sender_account,
                reply_to_chat_id: event.reply_to_chat_id.filter(|chat| !chat.is_empty()),
            }),
        })),
        Some(pb::client_frame::Body::MessageEdit(event)) => Ok(ConnectorInput::event(Event {
            source: SourceAddress {
                connector_id: connector_id.clone(),
                address: ChatAddress::new(
                    required_string(event.chat_id, "chat_id")?,
                    event.thread_id,
                ),
                chat_title: None,
            },
            payload: EventKind::Edit {
                native_id: required_string(event.platform_message_id, "platform_message_id")?,
                text: event.text,
            },
        })),
        Some(pb::client_frame::Body::MessageDelete(event)) => Ok(ConnectorInput::event(Event {
            source: SourceAddress {
                connector_id: connector_id.clone(),
                address: ChatAddress::new(
                    required_string(event.chat_id, "chat_id")?,
                    event.thread_id,
                ),
                chat_title: None,
            },
            payload: EventKind::Delete {
                native_id: required_string(event.platform_message_id, "platform_message_id")?,
            },
        })),
        Some(pb::client_frame::Body::Acknowledgement(acknowledgement)) => {
            Ok(ConnectorInput::Acknowledgement(DispatchAck {
                dispatch_id: wuphf_model::DispatchId(parse_uuid(
                    &acknowledgement.dispatch_id,
                    "dispatch_id",
                )?),
                native_id: required_string(
                    acknowledgement.platform_message_id,
                    "platform_message_id",
                )?,
                thread_id: acknowledgement.thread_id,
            }))
        }
        Some(pb::client_frame::Body::IdentityMigrated(migration)) => {
            Ok(ConnectorInput::Work(Box::new(InboundWork::Migration {
                connector_id: connector_id.clone(),
                old: ChatAddress::new(
                    required_string(migration.old_chat_id, "old_chat_id")?,
                    migration.old_thread_id,
                ),
                new: ChatAddress::new(
                    required_string(migration.new_chat_id, "new_chat_id")?,
                    migration.new_thread_id,
                ),
            })))
        }
        Some(pb::client_frame::Body::OwnerNotice(notice)) => {
            let kind = match pb::NoticeKind::try_from(notice.kind) {
                Ok(pb::NoticeKind::LoggedOut) => NoticeKind::LoggedOut,
                Ok(pb::NoticeKind::AuthRequired) => NoticeKind::AuthRequired,
                Ok(pb::NoticeKind::Degraded) => NoticeKind::Degraded,
                Ok(pb::NoticeKind::Recovered) => NoticeKind::Recovered,
                Ok(pb::NoticeKind::DisappearingTimerUnknown) => {
                    NoticeKind::DisappearingTimerUnknown
                }
                Ok(pb::NoticeKind::MediaUnavailable) => NoticeKind::MediaUnavailable,
                Ok(pb::NoticeKind::LinkRequired) => NoticeKind::LinkRequired,
                // An unset or unknown kind still means the connector is trying
                // to say something is wrong, so it is reported as a degradation
                // rather than dropped.
                Ok(pb::NoticeKind::Unspecified) | Err(_) => NoticeKind::Degraded,
            };
            Ok(ConnectorInput::Work(Box::new(InboundWork::Notice {
                connector_id: connector_id.clone(),
                kind,
                detail: notice.detail,
                media_ids: notice
                    .media_ids
                    .iter()
                    .map(|id| parse_uuid(id, "owner_notice.media_ids").map(MediaId))
                    .collect::<Result<_, _>>()?,
            })))
        }
        Some(pb::client_frame::Body::DispatchFailed(failure)) => {
            Ok(ConnectorInput::DispatchFailed(DispatchFailed {
                dispatch_id: wuphf_model::DispatchId(parse_uuid(
                    &failure.dispatch_id,
                    "dispatch_id",
                )?),
                reason: failure.reason,
            }))
        }
        Some(pb::client_frame::Body::Registration(_)) => {
            Err(WireError("registration already completed".into()))
        }
        // Settled by the session before frames reach here.
        Some(pb::client_frame::Body::CommandResult(_)) => {
            Err(WireError("a command result outside a session".into()))
        }
        Some(pb::client_frame::Body::ProbeResult(_)) => {
            Err(WireError("a probe result outside a session".into()))
        }
        Some(pb::client_frame::Body::ChatUpdate(event)) => {
            Ok(ConnectorInput::Work(Box::new(InboundWork::ChatUpdate {
                connector_id: connector_id.clone(),
                snapshot: event.snapshot,
                updates: event
                    .updates
                    .into_iter()
                    .map(chat_update_from_proto)
                    .collect::<Result<_, WireError>>()?,
            })))
        }
        Some(pb::client_frame::Body::Reaction(event)) => {
            Ok(ConnectorInput::event(Event {
                source: SourceAddress {
                    connector_id: connector_id.clone(),
                    address: ChatAddress::new(
                        required_string(event.chat_id, "chat_id")?,
                        event.thread_id,
                    ),
                    chat_title: None,
                },
                payload: EventKind::Reaction {
                    native_id: required_string(event.platform_message_id, "platform_message_id")?,
                    reactor: required_string(event.sender, "sender")?,
                    reactor_name: event.sender_name,
                    // An empty emoji is the toggle's "cleared" state.
                    emoji: (!event.emoji.is_empty()).then_some(event.emoji),
                    from_me: event.from_me,
                },
            }))
        }
        Some(pb::client_frame::Body::ReactionsSet(event)) => Ok(ConnectorInput::event(Event {
            source: SourceAddress {
                connector_id: connector_id.clone(),
                address: ChatAddress::new(
                    required_string(event.chat_id, "chat_id")?,
                    event.thread_id,
                ),
                chat_title: None,
            },
            payload: EventKind::ReactionSet {
                native_id: required_string(event.platform_message_id, "platform_message_id")?,
                reactions: event
                    .reactions
                    .into_iter()
                    .map(|reaction| {
                        Ok(ReactorReaction {
                            reactor: required_string(reaction.sender, "sender")?,
                            reactor_name: reaction.sender_name,
                            emoji: required_string(reaction.emoji, "emoji")?,
                        })
                    })
                    .collect::<Result<_, WireError>>()?,
            },
        })),
        Some(pb::client_frame::Body::Receipt(event)) => {
            let kind = match pb::ReceiptKind::try_from(event.kind)
                .map_err(|_| WireError("invalid receipt kind".into()))?
            {
                pb::ReceiptKind::Delivered => ReceiptKind::Delivered,
                pb::ReceiptKind::Read => ReceiptKind::Read,
                pb::ReceiptKind::Unspecified => {
                    return Err(WireError("receipt kind required".into()));
                }
            };
            Ok(ConnectorInput::event(Event {
                source: SourceAddress {
                    connector_id: connector_id.clone(),
                    address: ChatAddress::new(
                        required_string(event.chat_id, "chat_id")?,
                        event.thread_id,
                    ),
                    chat_title: None,
                },
                payload: EventKind::Receipt {
                    native_id: required_string(event.platform_message_id, "platform_message_id")?,
                    observer: required_string(event.observer, "observer")?,
                    from_me: event.from_me,
                    kind,
                },
            }))
        }
        None => Err(WireError("client frame body required".into())),
    }
}

fn output_to_proto(output: ConnectorOutput) -> pb::ServerFrame {
    let body = match output {
        ConnectorOutput::Dispatch(dispatch) => {
            let dispatch_id = dispatch.id.to_string();
            let target = dispatch.to;
            match dispatch.action {
                Action::Send {
                    message,
                    reply: reply_context,
                    disappearing,
                } => {
                    let container = target.address.chat_id.clone();
                    let reply_to_native_id = reply_context
                        .as_ref()
                        .map(|context| context.native_id.clone());
                    pb::server_frame::Body::MessageCreate(Box::new(pb::MessageCreateDispatch {
                        dispatch_id,
                        target_chat_id: target.address.chat_id,
                        target_thread_id: target.address.thread_id,
                        // A thread is always created inside the target chat.
                        provision_thread: target
                            .create_thread
                            .map(|title| pb::ProvisionThread { container, title }),
                        sender: message.author_id,
                        participant: message.participant,
                        text: message.text,
                        attachments: message
                            .attachments
                            .into_iter()
                            .map(attachment_to_proto)
                            .collect(),
                        reply_to_platform_message_id: reply_to_native_id,
                        timestamp_ms: message.timestamp_ms,
                        reply_context: reply_context.map(reply_context_to_proto),
                        sender_name: message.author_name,
                        disappearing: disappearing.map(timer_to_proto),
                        system_note: false,
                        post: false,
                    }))
                }
                // A note is a plain create with no author, reply or timer; the
                // connector renders it without a sender label.
                // A post is a plain create flagged `post`: no quote, no timer
                // (a story keeps its own 24 hours).
                Action::Post { message } => {
                    pb::server_frame::Body::MessageCreate(Box::new(pb::MessageCreateDispatch {
                        dispatch_id,
                        target_chat_id: target.address.chat_id,
                        target_thread_id: target.address.thread_id,
                        provision_thread: None,
                        sender: message.author_id,
                        participant: message.participant,
                        text: message.text,
                        attachments: message
                            .attachments
                            .into_iter()
                            .map(attachment_to_proto)
                            .collect(),
                        reply_to_platform_message_id: None,
                        timestamp_ms: message.timestamp_ms,
                        reply_context: None,
                        sender_name: message.author_name,
                        disappearing: None,
                        system_note: false,
                        post: true,
                    }))
                }
                Action::OpenPanel { actor } => {
                    pb::server_frame::Body::OpenPanel(pb::OpenPanelDispatch {
                        dispatch_id,
                        target_chat_id: target.address.chat_id,
                        target_thread_id: target.address.thread_id,
                        actor,
                    })
                }
                Action::Note { text, attachments } => {
                    let container = target.address.chat_id.clone();
                    pb::server_frame::Body::MessageCreate(Box::new(pb::MessageCreateDispatch {
                        dispatch_id,
                        target_chat_id: target.address.chat_id,
                        target_thread_id: target.address.thread_id,
                        provision_thread: target
                            .create_thread
                            .map(|title| pb::ProvisionThread { container, title }),
                        sender: String::new(),
                        participant: None,
                        text: Some(text),
                        attachments: attachments.into_iter().map(attachment_to_proto).collect(),
                        reply_to_platform_message_id: None,
                        timestamp_ms: 0,
                        reply_context: None,
                        sender_name: None,
                        disappearing: None,
                        system_note: true,
                        post: false,
                    }))
                }
                Action::Edit { message, native_id } => {
                    pb::server_frame::Body::MessageEdit(pb::MessageEditDispatch {
                        dispatch_id,
                        target_chat_id: target.address.chat_id,
                        target_thread_id: target.address.thread_id,
                        target_platform_message_id: native_id,
                        text: message.text,
                        sender_name: message.author_name,
                        participant: message.participant,
                    })
                }
                Action::Delete { native_id, .. } => {
                    pb::server_frame::Body::MessageDelete(pb::MessageDeleteDispatch {
                        dispatch_id,
                        target_chat_id: target.address.chat_id,
                        target_thread_id: target.address.thread_id,
                        target_platform_message_id: native_id,
                    })
                }
                Action::SetChatState {
                    pinned,
                    mute,
                    archived,
                } => pb::server_frame::Body::ChatState(pb::ChatStateDispatch {
                    dispatch_id,
                    target_chat_id: target.address.chat_id,
                    target_thread_id: target.address.thread_id,
                    pinned,
                    mute: mute.map(|mute| pb::MuteState {
                        muted_until: mute.muted_until,
                    }),
                    archived,
                }),
                Action::RenameThread { title, .. } => {
                    pb::server_frame::Body::MessageRetitle(pb::MessageRetitleDispatch {
                        dispatch_id,
                        target_chat_id: target.address.chat_id,
                        target_thread_id: target.address.thread_id.unwrap_or_default(),
                        title,
                    })
                }
                Action::React {
                    native_id,
                    reactor,
                    reactor_name,
                    emoji,
                    by_owner,
                    target_author,
                    ..
                } => pb::server_frame::Body::Reaction(pb::ReactionDispatch {
                    dispatch_id,
                    target_chat_id: target.address.chat_id,
                    target_thread_id: target.address.thread_id,
                    target_platform_message_id: native_id,
                    reactor,
                    reactor_name,
                    emoji: emoji.unwrap_or_default(),
                    by_owner,
                    target_sender: target_author.author_id,
                    target_participant: target_author.participant,
                    target_authored_by_bridge: target_author.authored_by_bridge,
                }),
                Action::Refuse {
                    native_id,
                    reason,
                    note_ttl_secs,
                    ..
                } => pb::server_frame::Body::Refused(pb::MessageRefusedDispatch {
                    dispatch_id,
                    target_chat_id: target.address.chat_id,
                    target_thread_id: target.address.thread_id,
                    target_platform_message_id: native_id,
                    reason,
                    note_ttl_secs,
                }),
                Action::Receipt {
                    native_id,
                    kind: ReceiptKind::Delivered,
                    ..
                } => pb::server_frame::Body::DeliveryAck(pb::DeliveryAckDispatch {
                    dispatch_id,
                    target_chat_id: target.address.chat_id,
                    target_thread_id: target.address.thread_id,
                    target_platform_message_id: native_id,
                }),
                Action::Receipt {
                    native_id,
                    kind: ReceiptKind::Read,
                    ..
                } => pb::server_frame::Body::SeenReceipt(pb::SeenReceiptDispatch {
                    dispatch_id,
                    target_chat_id: target.address.chat_id,
                    target_thread_id: target.address.thread_id,
                    target_platform_message_id: native_id,
                }),
            }
        }
        ConnectorOutput::Rejected { reason } => {
            pb::server_frame::Body::Rejected(pb::Rejected { reason })
        }
    };
    pb::ServerFrame { body: Some(body) }
}

fn reply_context_to_proto(context: ReplyContext) -> pb::ReplyContext {
    pb::ReplyContext {
        logical_message_id: context.message_id.to_string(),
        platform_message_id: context.native_id,
        sender: context.author_id,
        participant: context.participant,
        text: context.text,
        attachments: context
            .attachments
            .into_iter()
            .map(attachment_to_proto)
            .collect(),
        authored_by_bridge: context.authored_by_bridge,
        chat_id: context.chat_id,
    }
}

fn chat_update_from_proto(update: pb::ChatUpdate) -> Result<ChatUpdate, WireError> {
    Ok(ChatUpdate {
        chat_id: required_string(update.chat_id, "chat_id")?,
        disappearing: update.disappearing.map(timer_from_proto),
        title: update.title,
        description: update.description,
        avatar: update
            .avatar
            .map(|avatar| {
                Ok::<_, WireError>(ReportedAvatar {
                    id: avatar.id,
                    media_id: avatar
                        .media_id
                        .map(|id| parse_uuid(&id, "avatar media_id").map(MediaId))
                        .transpose()?,
                })
            })
            .transpose()?,
        participant_count: update.participant_count,
        participant_changes: update
            .participant_changes
            .into_iter()
            .map(participant_change_from_proto)
            .collect::<Result<_, _>>()?,
        thread_id: update.thread_id.filter(|thread| !thread.is_empty()),
        pinned: update.pinned,
        mute: update.mute.map(|mute| MuteState {
            muted_until: mute.muted_until,
        }),
        archived: update.archived,
        posting: update.posting.map(|posting| ChatPosting {
            messages: posting.messages,
            reactions: posting.reactions,
            private_replies: posting.private_replies,
            posts: posting.posts,
            audience: posting.audience,
            post_edits: posting.post_edits,
        }),
    })
}

fn participant_change_from_proto(
    change: pb::ParticipantChange,
) -> Result<ParticipantChange, WireError> {
    use pb::participant_change::Kind;
    let kind = match Kind::try_from(change.kind)
        .map_err(|_| WireError("invalid participant change kind".into()))?
    {
        Kind::Joined => ParticipantChangeKind::Joined,
        Kind::Left => ParticipantChangeKind::Left,
        Kind::Promoted => ParticipantChangeKind::Promoted,
        Kind::Demoted => ParticipantChangeKind::Demoted,
        Kind::Unspecified => return Err(WireError("participant change kind required".into())),
    };
    Ok(ParticipantChange {
        kind,
        participant: required_string(change.participant, "participant")?,
        name: change.name,
        actor: change.actor,
        actor_name: change.actor_name,
    })
}

fn timer_from_proto(timer: pb::DisappearingTimer) -> DisappearingTimer {
    DisappearingTimer {
        seconds: timer.seconds,
        setting_timestamp: timer.setting_timestamp,
    }
}

fn timer_to_proto(timer: DisappearingTimer) -> pb::DisappearingTimer {
    pb::DisappearingTimer {
        seconds: timer.seconds,
        setting_timestamp: timer.setting_timestamp,
    }
}

fn attachment_from_proto(attachment: pb::Attachment) -> Result<Attachment, WireError> {
    let kind = match pb::attachment::Kind::try_from(attachment.kind)
        .map_err(|_| WireError("invalid attachment kind".into()))?
    {
        pb::attachment::Kind::Image => AttachmentKind::Image,
        pb::attachment::Kind::Video => AttachmentKind::Video,
        pb::attachment::Kind::Audio => AttachmentKind::Audio,
        pb::attachment::Kind::File => AttachmentKind::File,
        pb::attachment::Kind::Sticker => AttachmentKind::Sticker,
        pb::attachment::Kind::Unspecified => {
            return Err(WireError("attachment kind required".into()));
        }
    };
    Ok(Attachment {
        media_id: MediaId(parse_uuid(&attachment.media_id, "attachment.media_id")?),
        kind,
        file_name: attachment.file_name,
        mime_type: attachment.mime_type,
        size_bytes: attachment.size_bytes,
        animated: attachment.animated,
    })
}

pub(crate) fn attachment_to_proto(attachment: Attachment) -> pb::Attachment {
    let kind = match attachment.kind {
        AttachmentKind::Image => pb::attachment::Kind::Image,
        AttachmentKind::Video => pb::attachment::Kind::Video,
        AttachmentKind::Audio => pb::attachment::Kind::Audio,
        AttachmentKind::File => pb::attachment::Kind::File,
        AttachmentKind::Sticker => pb::attachment::Kind::Sticker,
    };
    pb::Attachment {
        media_id: attachment.media_id.to_string(),
        kind: kind.into(),
        file_name: attachment.file_name,
        mime_type: attachment.mime_type,
        size_bytes: attachment.size_bytes,
        animated: attachment.animated,
    }
}

fn platform_from_proto(value: i32) -> Result<Platform, WireError> {
    match pb::Platform::try_from(value).map_err(|_| WireError("invalid platform".into()))? {
        pb::Platform::Whatsapp => Ok(Platform::WhatsApp),
        pb::Platform::Telegram => Ok(Platform::Telegram),
        pb::Platform::Discord => Ok(Platform::Discord),
        pb::Platform::Signal => Ok(Platform::Signal),
        pb::Platform::Slack => Ok(Platform::Slack),
        pb::Platform::Matrix => Ok(Platform::Matrix),
        pb::Platform::Instagram => Ok(Platform::Instagram),
        pb::Platform::Unspecified => Err(WireError("platform required".into())),
    }
}

fn parse_uuid(value: &str, name: &str) -> Result<Uuid, WireError> {
    Uuid::parse_str(value).map_err(|_| WireError(format!("{name} must be valid UUID")))
}

fn required_string(value: String, name: &str) -> Result<String, WireError> {
    if value.is_empty() {
        Err(WireError(format!("{name} required")))
    } else {
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wuphf_model::{ConversationId, Dispatch, EndpointId, Message, MessageId, Target};

    fn dispatch(action: Action, create_thread: Option<String>) -> Dispatch {
        Dispatch::new(
            Target {
                endpoint_id: EndpointId::new(),
                connector_id: ConnectorId("telegram".into()),
                address: ChatAddress::new("tg-chat", None),
                create_thread,
            },
            ConversationId::new(),
            action,
            None,
        )
    }

    fn body(dispatch: Dispatch) -> pb::server_frame::Body {
        output_to_proto(ConnectorOutput::Dispatch(Box::new(dispatch)))
            .body
            .unwrap()
    }

    fn receipt(kind: ReceiptKind) -> Action {
        Action::Receipt {
            message_id: MessageId::new(),
            native_id: "wa-1".into(),
            kind,
        }
    }

    /// The model folds delivery acks and seen receipts into one action; the
    /// wire keeps a frame for each, chosen by the receipt kind.
    #[test]
    fn receipts_map_to_their_own_frames() {
        let pb::server_frame::Body::DeliveryAck(ack) =
            body(dispatch(receipt(ReceiptKind::Delivered), None))
        else {
            panic!("a delivered receipt is a DeliveryAck frame");
        };
        assert_eq!(ack.target_platform_message_id, "wa-1");
        assert!(matches!(
            body(dispatch(receipt(ReceiptKind::Read), None)),
            pb::server_frame::Body::SeenReceipt(_)
        ));
    }

    /// A thread to create travels as a title in the model; the wire frame names
    /// its container too, which is always the target chat.
    #[test]
    fn a_thread_to_create_is_provisioned_inside_the_target_chat() {
        let message = Message {
            id: MessageId::new(),
            conversation_id: ConversationId::new(),
            author_id: "priya".into(),
            participant: None,
            author_name: None,
            text: Some("hi".into()),
            attachments: Vec::new(),
            reply_to: None,
            timestamp_ms: 0,
            deleted: false,
        };
        let send = Action::Send {
            message,
            reply: None,
            disappearing: None,
        };
        let pb::server_frame::Body::MessageCreate(create) =
            body(dispatch(send, Some("Priya".into())))
        else {
            panic!("a send is a MessageCreate frame");
        };
        assert_eq!(create.target_chat_id, "tg-chat");
        assert_eq!(
            create.provision_thread,
            Some(pb::ProvisionThread {
                container: "tg-chat".into(),
                title: "Priya".into(),
            })
        );
    }

    /// A reaction carries whether it is the owner's and who wrote the reacted
    /// copy, which WhatsApp needs to address it.
    #[test]
    fn an_owners_reaction_carries_the_target_author() {
        let react = Action::React {
            message_id: MessageId::new(),
            native_id: "wa-1".into(),
            reactor: "owner".into(),
            reactor_name: None,
            emoji: Some("❤️".into()),
            by_owner: true,
            target_author: wuphf_model::MessageAuthor {
                author_id: "alice@s.whatsapp.net".into(),
                participant: Some("alice@lid".into()),
                authored_by_bridge: false,
            },
            confirm: None,
        };
        let pb::server_frame::Body::Reaction(reaction) = body(dispatch(react, None)) else {
            panic!("a reaction is a Reaction frame");
        };
        assert!(reaction.by_owner);
        assert_eq!(reaction.target_sender, "alice@s.whatsapp.net");
        assert_eq!(reaction.target_participant.as_deref(), Some("alice@lid"));
        assert!(!reaction.target_authored_by_bridge);
    }

    /// A message's whole reaction set decodes reactor by reactor; a reactor
    /// with no emoji is a wire error, since an absent reactor means none.
    #[test]
    fn a_reaction_set_decodes_each_reactor() {
        let set = |emoji: &str| pb::ReactionsEvent {
            chat_id: "-100".into(),
            thread_id: Some("7".into()),
            platform_message_id: "tg-1".into(),
            reactions: vec![pb::ReactorReaction {
                sender: "42".into(),
                sender_name: Some("Owner".into()),
                emoji: emoji.into(),
            }],
        };
        let frame = |body| pb::ClientFrame { body: Some(body) };
        let input = input_from_proto(
            frame(pb::client_frame::Body::ReactionsSet(set("❤️"))),
            &whatsapp(),
        )
        .unwrap();
        let ConnectorInput::Work(work) = input else {
            panic!("expected an event, got {input:?}");
        };
        let InboundWork::Event(event) = *work else {
            panic!("expected an event");
        };
        assert_eq!(event.source.address.thread_id.as_deref(), Some("7"));
        assert_eq!(
            event.payload,
            EventKind::ReactionSet {
                native_id: "tg-1".into(),
                reactions: vec![ReactorReaction {
                    reactor: "42".into(),
                    reactor_name: Some("Owner".into()),
                    emoji: "❤️".into(),
                }],
            }
        );
        assert!(
            input_from_proto(
                frame(pb::client_frame::Body::ReactionsSet(set(""))),
                &whatsapp()
            )
            .is_err()
        );
    }

    fn whatsapp() -> ConnectorId {
        ConnectorId("whatsapp".into())
    }

    fn work(frame: pb::client_frame::Body) -> InboundWork {
        let input = input_from_proto(pb::ClientFrame { body: Some(frame) }, &whatsapp()).unwrap();
        let ConnectorInput::Work(work) = input else {
            panic!("expected inbound work, got {input:?}");
        };
        *work
    }

    fn send_with(disappearing: Option<DisappearingTimer>) -> pb::MessageCreateDispatch {
        let message = Message {
            id: MessageId::new(),
            conversation_id: ConversationId::new(),
            author_id: "priya".into(),
            participant: None,
            author_name: None,
            text: Some("hi".into()),
            attachments: Vec::new(),
            reply_to: None,
            timestamp_ms: 0,
            deleted: false,
        };
        let send = Action::Send {
            message,
            reply: None,
            disappearing,
        };
        let pb::server_frame::Body::MessageCreate(create) = body(dispatch(send, None)) else {
            panic!("a send is a MessageCreate frame");
        };
        *create
    }

    #[test]
    fn every_chat_update_field_crosses_the_wire() {
        let media = MediaId::new();
        let frame = pb::client_frame::Body::ChatUpdate(pb::ChatUpdateEvent {
            updates: vec![pb::ChatUpdate {
                chat_id: "1@g.us".into(),
                title: Some("[G] Family".into()),
                description: Some(String::new()),
                avatar: Some(pb::ChatAvatar {
                    id: "pic-1".into(),
                    media_id: Some(media.to_string()),
                }),
                participant_count: Some(12),
                participant_changes: vec![
                    pb::ParticipantChange {
                        kind: pb::participant_change::Kind::Joined as i32,
                        participant: "2@s.whatsapp.net".into(),
                        name: Some("Bob".into()),
                        actor: Some("3@s.whatsapp.net".into()),
                        actor_name: Some("Alice".into()),
                    },
                    pb::ParticipantChange {
                        kind: pb::participant_change::Kind::Demoted as i32,
                        participant: "4@s.whatsapp.net".into(),
                        name: None,
                        actor: None,
                        actor_name: None,
                    },
                ],
                ..Default::default()
            }],
            snapshot: true,
        });
        assert_eq!(
            work(frame),
            InboundWork::ChatUpdate {
                connector_id: whatsapp(),
                snapshot: true,
                updates: vec![ChatUpdate {
                    title: Some("[G] Family".into()),
                    description: Some(String::new()),
                    avatar: Some(ReportedAvatar {
                        id: "pic-1".into(),
                        media_id: Some(media),
                    }),
                    participant_count: Some(12),
                    participant_changes: vec![
                        ParticipantChange {
                            kind: ParticipantChangeKind::Joined,
                            participant: "2@s.whatsapp.net".into(),
                            name: Some("Bob".into()),
                            actor: Some("3@s.whatsapp.net".into()),
                            actor_name: Some("Alice".into()),
                        },
                        ParticipantChange {
                            kind: ParticipantChangeKind::Demoted,
                            participant: "4@s.whatsapp.net".into(),
                            name: None,
                            actor: None,
                            actor_name: None,
                        },
                    ],
                    ..ChatUpdate::new("1@g.us")
                }],
            }
        );
    }

    #[test]
    fn malformed_profile_reports_are_wire_errors() {
        let rejected = |update: pb::ChatUpdate| {
            input_from_proto(
                pb::ClientFrame {
                    body: Some(pb::client_frame::Body::ChatUpdate(pb::ChatUpdateEvent {
                        updates: vec![update],
                        snapshot: false,
                    })),
                },
                &whatsapp(),
            )
            .is_err()
        };
        let change = |kind: pb::participant_change::Kind, participant: &str| pb::ChatUpdate {
            chat_id: "1@g.us".into(),
            participant_changes: vec![pb::ParticipantChange {
                kind: kind as i32,
                participant: participant.into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert!(rejected(change(
            pb::participant_change::Kind::Unspecified,
            "2@s.whatsapp.net"
        )));
        assert!(rejected(change(pb::participant_change::Kind::Left, "")));
        assert!(rejected(pb::ChatUpdate {
            chat_id: "1@g.us".into(),
            avatar: Some(pb::ChatAvatar {
                id: "pic-1".into(),
                media_id: Some("not-a-uuid".into()),
            }),
            ..Default::default()
        }));
    }

    #[test]
    fn a_note_is_a_plain_unlabelled_create() {
        let photo = Attachment {
            media_id: MediaId::new(),
            kind: AttachmentKind::Image,
            file_name: Some("avatar.jpg".into()),
            mime_type: Some("image/jpeg".into()),
            size_bytes: Some(3),
            animated: false,
        };
        let note = Action::Note {
            text: "🖼️ Photo changed".into(),
            attachments: vec![photo],
        };
        let pb::server_frame::Body::MessageCreate(create) =
            body(dispatch(note, Some("[G] Family".into())))
        else {
            panic!("a note is a MessageCreate frame");
        };
        assert!(create.system_note);
        assert_eq!(create.text.as_deref(), Some("🖼️ Photo changed"));
        assert_eq!(create.attachments.len(), 1);
        assert_eq!(create.sender, "");
        assert_eq!(create.participant, None);
        assert_eq!(create.sender_name, None);
        assert_eq!(create.reply_to_platform_message_id, None);
        assert_eq!(create.reply_context, None);
        assert_eq!(create.disappearing, None);
        assert_eq!(
            create.provision_thread,
            Some(pb::ProvisionThread {
                container: "tg-chat".into(),
                title: "[G] Family".into(),
            })
        );
        // An ordinary send is never a note.
        assert!(!send_with(None).system_note);
    }

    #[test]
    fn chat_state_reports_cross_the_wire() {
        let frame = pb::client_frame::Body::ChatUpdate(pb::ChatUpdateEvent {
            updates: vec![
                pb::ChatUpdate {
                    chat_id: "telegram:channel:1".into(),
                    thread_id: Some("57".into()),
                    pinned: Some(true),
                    mute: Some(pb::MuteState { muted_until: -1 }),
                    archived: Some(false),
                    ..Default::default()
                },
                // An empty thread id is no thread.
                pb::ChatUpdate {
                    chat_id: "1@g.us".into(),
                    thread_id: Some(String::new()),
                    pinned: Some(false),
                    ..Default::default()
                },
            ],
            snapshot: false,
        });
        let InboundWork::ChatUpdate { updates, .. } = work(frame) else {
            panic!("a chat update");
        };
        assert_eq!(
            updates[0],
            ChatUpdate {
                thread_id: Some("57".into()),
                pinned: Some(true),
                mute: Some(MuteState { muted_until: -1 }),
                archived: Some(false),
                ..ChatUpdate::new("telegram:channel:1")
            }
        );
        assert_eq!(updates[1].thread_id, None);
    }

    #[test]
    fn a_chat_state_change_is_a_chat_state_frame() {
        let mut topic = dispatch(
            Action::SetChatState {
                pinned: None,
                mute: Some(MuteState {
                    muted_until: 1_900_000_000,
                }),
                archived: Some(true),
            },
            None,
        );
        topic.to.address = ChatAddress::new("tg-chat", Some("57".into()));
        let pb::server_frame::Body::ChatState(state) = body(topic.clone()) else {
            panic!("a chat-state change is a ChatState frame");
        };
        assert_eq!(
            state,
            pb::ChatStateDispatch {
                dispatch_id: topic.id.to_string(),
                target_chat_id: "tg-chat".into(),
                target_thread_id: Some("57".into()),
                pinned: None,
                mute: Some(pb::MuteState {
                    muted_until: 1_900_000_000
                }),
                archived: Some(true),
            }
        );
    }

    #[test]
    fn chat_update_frames_become_inbound_work() {
        let frame = pb::client_frame::Body::ChatUpdate(pb::ChatUpdateEvent {
            updates: vec![
                pb::ChatUpdate {
                    chat_id: "1@g.us".into(),
                    disappearing: Some(pb::DisappearingTimer {
                        seconds: 604_800,
                        setting_timestamp: Some(1_700_000_000),
                    }),
                    ..Default::default()
                },
                pb::ChatUpdate {
                    chat_id: "2@s.whatsapp.net".into(),
                    disappearing: None,
                    ..Default::default()
                },
            ],
            snapshot: false,
        });
        assert_eq!(
            work(frame),
            InboundWork::ChatUpdate {
                connector_id: whatsapp(),
                snapshot: false,
                updates: vec![
                    ChatUpdate {
                        chat_id: "1@g.us".into(),
                        disappearing: Some(DisappearingTimer {
                            seconds: 604_800,
                            setting_timestamp: Some(1_700_000_000),
                        }),
                        ..ChatUpdate::default()
                    },
                    ChatUpdate {
                        chat_id: "2@s.whatsapp.net".into(),
                        disappearing: None,
                        ..ChatUpdate::default()
                    },
                ],
            }
        );

        let empty_chat = pb::client_frame::Body::ChatUpdate(pb::ChatUpdateEvent {
            updates: vec![pb::ChatUpdate {
                chat_id: String::new(),
                disappearing: None,
                ..Default::default()
            }],
            snapshot: false,
        });
        assert!(
            input_from_proto(
                pb::ClientFrame {
                    body: Some(empty_chat)
                },
                &whatsapp()
            )
            .is_err()
        );
    }

    #[test]
    fn a_create_event_carries_its_timer() {
        let frame = pb::client_frame::Body::MessageCreate(pb::MessageCreateEvent {
            chat_id: "1@s.whatsapp.net".into(),
            thread_id: None,
            chat_title: None,
            platform_message_id: "wa-1".into(),
            sender: "1@s.whatsapp.net".into(),
            participant: None,
            text: Some("hi".into()),
            attachments: vec![],
            reply_to_platform_message_id: None,
            timestamp_ms: 1,
            sender_name: None,
            disappearing: Some(pb::DisappearingTimer {
                seconds: 86_400,
                setting_timestamp: None,
            }),
            sender_account: None,
            reply_to_chat_id: None,
        });
        let InboundWork::Event(event) = work(frame) else {
            panic!("a create is an event");
        };
        let EventKind::NewMessage(message) = event.payload else {
            panic!("a create is a new message");
        };
        assert_eq!(
            message.disappearing,
            Some(DisappearingTimer {
                seconds: 86_400,
                setting_timestamp: None,
            })
        );
    }

    #[test]
    fn a_send_carries_the_target_timer_to_the_wire() {
        let timer = DisappearingTimer {
            seconds: 604_800,
            setting_timestamp: Some(1_700_000_000),
        };
        assert_eq!(
            send_with(Some(timer)).disappearing,
            Some(pb::DisappearingTimer {
                seconds: 604_800,
                setting_timestamp: Some(1_700_000_000),
            })
        );
        assert_eq!(send_with(None).disappearing, None);
    }

    #[test]
    fn the_timer_notice_kind_maps_to_its_own_kind() {
        let frame = pb::client_frame::Body::OwnerNotice(pb::OwnerNotice {
            kind: pb::NoticeKind::DisappearingTimerUnknown.into(),
            detail: "chat 1@s.whatsapp.net".into(),
            media_ids: vec![],
        });
        assert_eq!(
            work(frame),
            InboundWork::Notice {
                connector_id: whatsapp(),
                kind: NoticeKind::DisappearingTimerUnknown,
                detail: "chat 1@s.whatsapp.net".into(),
                media_ids: vec![],
            }
        );
    }

    #[test]
    fn the_media_notice_kind_maps_to_its_own_kind() {
        let frame = pb::client_frame::Body::OwnerNotice(pb::OwnerNotice {
            kind: pb::NoticeKind::MediaUnavailable.into(),
            detail: "image from Alice".into(),
            media_ids: vec![],
        });
        assert_eq!(
            work(frame),
            InboundWork::Notice {
                connector_id: whatsapp(),
                kind: NoticeKind::MediaUnavailable,
                detail: "image from Alice".into(),
                media_ids: vec![],
            }
        );
    }

    #[test]
    fn the_link_notice_kind_carries_its_media() {
        let qr = Uuid::new_v4();
        let frame = pb::client_frame::Body::OwnerNotice(pb::OwnerNotice {
            kind: pb::NoticeKind::LinkRequired.into(),
            detail: "scan the QR code".into(),
            media_ids: vec![qr.to_string()],
        });
        assert_eq!(
            work(frame),
            InboundWork::Notice {
                connector_id: whatsapp(),
                kind: NoticeKind::LinkRequired,
                detail: "scan the QR code".into(),
                media_ids: vec![MediaId(qr)],
            }
        );
        // A malformed id rejects the frame, as a malformed attachment does.
        let frame = pb::ClientFrame {
            body: Some(pb::client_frame::Body::OwnerNotice(pb::OwnerNotice {
                kind: pb::NoticeKind::LinkRequired.into(),
                detail: String::new(),
                media_ids: vec!["not-a-uuid".into()],
            })),
        };
        assert!(input_from_proto(frame, &whatsapp()).is_err());
    }

    #[test]
    fn registration_maps_the_disappearing_capability() {
        let connector = registration_from_proto(pb::Registration {
            connector_id: "whatsapp".into(),
            platform: pb::Platform::Whatsapp.into(),
            capabilities: Some(pb::Capabilities {
                supports_threads: false,
                supports_topic_create: false,
                supports_disappearing_timers: true,
            }),
            settings: None,
            commands: Vec::new(),
            tools: Vec::new(),
        })
        .unwrap();
        assert!(connector.capabilities.supports_disappearing_timers);
    }
}
