//! Helpers shared by both [`Storage`](crate::Storage) backends so the
//! dispatch-queue semantics stay identical across them.

use wuphf_model::{Action, Dispatch, DispatchState, Message, MessageCopy, ReplyContext};

/// The copy an acknowledged dispatch records at its target endpoint, or `None`
/// when settling it records no copy and releases nothing. Signals that act on
/// an existing copy (reactions, receipts) and thread renames settle without
/// one; this is the single source of that decision for both backends.
pub(crate) fn acknowledged_copy(dispatch: &Dispatch, native_id: &str) -> Option<MessageCopy> {
    if !dispatch.records_copy() {
        return None;
    }
    Some(MessageCopy {
        message_id: dispatch.message_id()?,
        endpoint_id: dispatch.to.endpoint_id,
        native_id: native_id.to_owned(),
    })
}

/// Release a dispatch that was blocked on its now-delivered parent: bind the
/// parent's native id into it and make it ready to send.
pub(crate) fn release_blocked(dispatch: &mut Dispatch, parent: Message, native_id: String) {
    bind_native_parent(dispatch, parent, native_id);
    dispatch.state = DispatchState::Pending;
}

/// Fill in the native parent id a released dispatch was waiting for. A reply
/// gains the quoted parent's id as its reply context; an edit, delete or signal
/// gains the native id of the copy it acts on at the target endpoint.
fn bind_native_parent(dispatch: &mut Dispatch, parent: Message, native_id: String) {
    match &mut dispatch.action {
        // A blocked reply is released precisely because the bridge just sent
        // the parent to this endpoint, so the quoted copy is always one the
        // bridge authored.
        Action::Send { reply, .. } => *reply = Some(ReplyContext::quoting(parent, native_id, true)),
        Action::Edit {
            native_id: target, ..
        }
        | Action::Delete {
            native_id: target, ..
        }
        | Action::React {
            native_id: target, ..
        }
        | Action::Receipt {
            native_id: target, ..
        } => *target = native_id,
        // A rename is never blocked on a parent message, so it is never
        // released with one.
        Action::RenameThread { .. } => {}
    }
}
