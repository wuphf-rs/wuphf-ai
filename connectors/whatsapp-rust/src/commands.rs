//! What the owner can run on this connector from the core's settings panel
//! (plan 058): a resync of every app-state collection, and a link code now
//! rather than at the next gap. Both answer at once; the work goes on in the
//! background.

use std::sync::Arc;

use wuphf_connector::Commands;
use wuphf_connector::commands::BoxFuture;
use wuphf_proto::connector as pb;

use crate::linking::Linking;
use crate::session::{CurrentResync, LinkedClient};

pub const RESYNC_APP_STATE: &str = "resync_app_state";
pub const FRESH_LINK_CODE: &str = "fresh_link_code";

/// The connector's commands, over the process's linking, linked client and
/// current session.
pub struct WhatsAppCommands {
    linking: Arc<Linking>,
    linked: LinkedClient,
    resync: CurrentResync,
}

impl WhatsAppCommands {
    pub fn new(linking: Arc<Linking>, linked: LinkedClient, resync: CurrentResync) -> Self {
        Self {
            linking,
            linked,
            resync,
        }
    }

    fn resync_app_state(&self) -> Result<String, String> {
        let client = self
            .linked
            .get()
            .map_err(|_| "WhatsApp is not linked; link it first".to_owned())?;
        let resync = self
            .resync
            .get()
            .ok_or_else(|| "the WhatsApp session is still starting; try again".to_owned())?;
        if !resync.all(client) {
            return Err("a resync is already running".into());
        }
        Ok("Resyncing contacts, pins, mutes and archives; changes arrive as they sync".into())
    }

    fn fresh_link_code(&self) -> String {
        if self.linked.get().is_ok() {
            return "WhatsApp is already linked".into();
        }
        self.linking.fresh_code();
        "A new link code is on its way (within a minute)".into()
    }
}

impl Commands for WhatsAppCommands {
    fn declarations(&self) -> Vec<pb::ConnectorCommand> {
        [
            (RESYNC_APP_STATE, "🔄 Resync app state"),
            (FRESH_LINK_CODE, "🔗 Send a new link code"),
        ]
        .into_iter()
        .map(|(id, title)| pb::ConnectorCommand {
            id: id.into(),
            title: title.into(),
        })
        .collect()
    }

    fn run<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            match id {
                RESYNC_APP_STATE => self.resync_app_state(),
                FRESH_LINK_CODE => Ok(self.fresh_link_code()),
                other => Err(format!("unknown command {other:?}")),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use tokio::sync::mpsc;
    use wuphf_connector::MediaClient;

    use super::*;
    use crate::config::LoginMethod;

    fn unlinked(method: LoginMethod) -> (WhatsAppCommands, Arc<Linking>) {
        let (inputs, _) = mpsc::channel(1);
        let media = MediaClient::connect("127.0.0.1:1", "t").unwrap();
        let linking = Arc::new(Linking::new(inputs, media, method));
        let commands = WhatsAppCommands::new(
            Arc::clone(&linking),
            LinkedClient::default(),
            CurrentResync::default(),
        );
        (commands, linking)
    }

    #[tokio::test]
    async fn both_commands_are_declared() {
        let (commands, _) = unlinked(LoginMethod::Qr);
        let ids: Vec<String> = commands
            .declarations()
            .into_iter()
            .map(|command| command.id)
            .collect();
        assert_eq!(ids, [RESYNC_APP_STATE, FRESH_LINK_CODE]);
    }

    #[tokio::test]
    async fn an_unlinked_session_cannot_resync() {
        let (commands, _) = unlinked(LoginMethod::Qr);
        let outcome = commands.run(RESYNC_APP_STATE).await;
        assert_eq!(outcome, Err("WhatsApp is not linked; link it first".into()));
    }

    #[tokio::test]
    async fn a_fresh_code_is_asked_for_at_once() {
        let (commands, linking) = unlinked(LoginMethod::PairCode);
        // A code was just asked for, so the next QR code would ask for none.
        assert!(linking.pair_code_ask().is_some());
        assert_eq!(linking.pair_code_ask(), None);
        let outcome = commands.run(FRESH_LINK_CODE).await;
        assert_eq!(
            outcome,
            Ok("A new link code is on its way (within a minute)".into())
        );
        let ask = linking.pair_code_ask().expect("a fresh code is asked for");
        assert!(ask.prompt_phone);
        assert!(ask.tell.is_some());
    }

    #[tokio::test]
    async fn an_unknown_command_is_refused() {
        let (commands, _) = unlinked(LoginMethod::Qr);
        assert!(commands.run("nope").await.is_err());
    }
}
