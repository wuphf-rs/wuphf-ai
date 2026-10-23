//! Helpers shared by both [`Storage`](crate::Storage) backends so the
//! dispatch-queue semantics stay identical across them.

use wuphf_model::{
    Action, Dispatch, DispatchCommand, DispatchState, Message, MessageCopy, MessageId, ReplyContext,
};

/// The state a freshly enqueued dispatch starts in: blocked while it waits for a
/// parent message to be delivered, otherwise ready to send.
pub(crate) fn dispatch_state(waiting_for: Option<MessageId>) -> DispatchState {
    match waiting_for {
        Some(waiting_for) => DispatchState::Blocked { waiting_for },
        None => DispatchState::Pending,
    }
}

/// The copy an acknowledged dispatch records at its target endpoint, or `None`
/// when settling it records no copy and releases nothing. Signals that act on
/// an existing copy (reactions, delivery acks, seen receipts) and retitles
/// settle without one; this is the single source of that decision for both
/// backends.
pub(crate) fn acknowledged_copy(
    command: &DispatchCommand,
    platform_message_id: &str,
) -> Option<MessageCopy> {
    if !command.records_copy() {
        return None;
    }
    Some(MessageCopy {
        message_id: command.message_id()?,
        endpoint_id: command.target_endpoint,
        platform_message_id: platform_message_id.to_owned(),
    })
}

/// Release a dispatch that was blocked on its now-delivered parent: bind the
/// parent's native id into it and make it ready to send.
pub(crate) fn release_blocked(
    dispatch: &mut Dispatch,
    parent: Message,
    platform_message_id: String,
) {
    bind_native_parent(&mut dispatch.command, parent, platform_message_id);
    dispatch.state = DispatchState::Pending;
}

/// Fill in the native parent id a released dispatch was waiting for. A reply
/// gains the quoted parent's id as its reply context; an edit gains the native
/// id of the copy it must revise at the target endpoint.
fn bind_native_parent(command: &mut DispatchCommand, parent: Message, platform_message_id: String) {
    match &mut command.payload {
        Action::MessageCreate { reply_context, .. } => {
            *reply_context = Some(ReplyContext {
                message_id: parent.id,
                platform_message_id,
                sender: parent.sender,
                participant: parent.participant,
                text: parent.text,
                attachments: parent.attachments,
                // A blocked reply is released precisely because the bridge just
                // sent the parent to this endpoint, so the quoted copy is
                // always one the bridge authored.
                authored_by_bridge: true,
            })
        }
        Action::MessageEdit {
            target_platform_message_id,
            ..
        }
        | Action::MessageDelete {
            target_platform_message_id,
            ..
        }
        | Action::Reaction {
            target_platform_message_id,
            ..
        }
        | Action::DeliveryAck {
            target_platform_message_id,
            ..
        }
        | Action::SeenReceipt {
            target_platform_message_id,
            ..
        } => *target_platform_message_id = platform_message_id,
        // A retitle is never blocked on a parent message, so it is never
        // released with one.
        Action::RetitleThread { .. } => {}
    }
}
