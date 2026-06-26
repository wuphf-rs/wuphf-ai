//! Helpers shared by both [`Storage`](crate::Storage) backends so the
//! dispatch-queue semantics stay identical across them.

use wuphf_model::{
    DispatchCommand, DispatchPayload, DispatchState, Message, MessageId, ReplyContext,
};

/// The state a freshly enqueued dispatch starts in: blocked while it waits for a
/// parent message to be delivered, otherwise ready to send.
pub(crate) fn dispatch_state(waiting_for: Option<MessageId>) -> DispatchState {
    match waiting_for {
        Some(waiting_for) => DispatchState::Blocked { waiting_for },
        None => DispatchState::Pending,
    }
}

/// Fill in the native parent id a released dispatch was waiting for. A reply
/// gains the quoted parent's id as its reply context; an edit gains the native
/// id of the copy it must revise at the target endpoint.
pub(crate) fn bind_native_parent(
    command: &mut DispatchCommand,
    parent: Message,
    platform_message_id: String,
) {
    match &mut command.payload {
        DispatchPayload::MessageCreate { reply_context, .. } => {
            *reply_context = Some(ReplyContext {
                message_id: parent.id,
                platform_message_id,
                sender: parent.sender,
                participant: parent.participant,
                text: parent.text,
                attachments: parent.attachments,
            })
        }
        DispatchPayload::MessageEdit {
            target_platform_message_id,
            ..
        }
        | DispatchPayload::MessageDelete {
            target_platform_message_id,
            ..
        }
        | DispatchPayload::Reaction {
            target_platform_message_id,
            ..
        }
        | DispatchPayload::DeliveryAck {
            target_platform_message_id,
            ..
        }
        | DispatchPayload::SeenReceipt {
            target_platform_message_id,
            ..
        } => *target_platform_message_id = platform_message_id,
        // A retitle is never blocked on a parent message, so it is never
        // released with one.
        DispatchPayload::RetitleThread { .. } => {}
    }
}
