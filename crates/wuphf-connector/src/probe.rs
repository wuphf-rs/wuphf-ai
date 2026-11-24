//! Health probes (plan 079): the core asks what this account can do in the
//! chats its rules use (`ProbeChats`), and the connector answers with facts
//! (`ProbeResult`). The core decides what is wrong; a connector only looks.

use std::sync::Arc;

use wuphf_proto::connector as pb;

use crate::commands::BoxFuture;

/// What a connector can find out about its chats. Answer within the core's
/// probe timeout (20 s); a fact it cannot learn stays `FACT_UNKNOWN`.
pub trait Prober: Send + Sync + 'static {
    /// The facts for each of `chat_ids`, in any order. A chat left out is
    /// unknown.
    fn probe<'a>(&'a self, chat_ids: &'a [String]) -> BoxFuture<'a, Vec<pb::ChatFacts>>;
}

/// The answer to `probe`: the facts, none without a prober (so every chat is
/// unknown). The Rust connectors run no external tools, so none are
/// reported.
pub(crate) async fn answer(
    prober: Option<&Arc<dyn Prober>>,
    probe: pb::ProbeChats,
) -> pb::ClientFrame {
    let chats = match prober {
        Some(prober) => prober.probe(&probe.chat_ids).await,
        None => Vec::new(),
    };
    pb::ClientFrame {
        body: Some(pb::client_frame::Body::ProbeResult(pb::ProbeResult {
            request_id: probe.request_id,
            chats,
            tools: Vec::new(),
        })),
    }
}
