//! The connector's lifetime: it ends with the core session or on shutdown,
//! never because the WhatsApp client stopped.

use std::future::Future;

/// How the connector stopped.
#[derive(Debug, PartialEq, Eq)]
pub enum Stop<E> {
    /// The core session returned, with its result.
    SessionEnded(Result<(), E>),
    /// The operator asked the connector to stop.
    Shutdown,
}

/// Runs until the core session returns or `shutdown` fires. The WhatsApp
/// client finishing does not end the connector: the library reconnects by
/// itself after every recoverable drop, so it finishes only when the session
/// is gone for good (a logout, a replaced stream, a ban, an outdated client).
/// The core session stays up so the notice saying why reaches the core and the
/// owner; `on_whatsapp_stopped` runs once, then the loop keeps waiting.
pub async fn supervise<S, W, Q, E>(
    session: S,
    whatsapp: W,
    shutdown: Q,
    on_whatsapp_stopped: impl FnOnce(),
) -> Stop<E>
where
    S: Future<Output = Result<(), E>>,
    W: Future,
    Q: Future,
{
    tokio::pin!(session, whatsapp, shutdown);
    let mut on_whatsapp_stopped = Some(on_whatsapp_stopped);
    loop {
        tokio::select! {
            result = &mut session => return Stop::SessionEnded(result),
            _ = &mut shutdown => return Stop::Shutdown,
            _ = &mut whatsapp, if on_whatsapp_stopped.is_some() => {
                if let Some(stopped) = on_whatsapp_stopped.take() {
                    stopped();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::future::{pending, ready};

    use tokio::sync::oneshot;

    use super::*;

    #[tokio::test]
    async fn the_whatsapp_client_stopping_does_not_stop_the_connector() {
        let calls = Cell::new(0);
        let (end, ended) = oneshot::channel::<()>();
        let stop = supervise(
            async {
                ended.await.ok();
                Ok::<(), ()>(())
            },
            ready(()),
            pending::<()>(),
            || {
                calls.set(calls.get() + 1);
                // The core session ends only after WhatsApp has stopped, so
                // the connector must still be waiting for it.
                end.send(()).unwrap();
            },
        )
        .await;
        assert_eq!(stop, Stop::SessionEnded(Ok(())));
        assert_eq!(calls.get(), 1);
    }

    #[tokio::test]
    async fn the_session_ending_ends_the_connector() {
        let stop = supervise(
            ready(Ok::<(), ()>(())),
            pending::<()>(),
            pending::<()>(),
            || panic!("WhatsApp did not stop"),
        )
        .await;
        assert_eq!(stop, Stop::SessionEnded(Ok(())));
    }

    #[tokio::test]
    async fn shutdown_wins_while_everything_runs() {
        let calls = Cell::new(0);
        let stop = supervise(
            pending::<Result<(), ()>>(),
            pending::<()>(),
            ready(()),
            || calls.set(calls.get() + 1),
        )
        .await;
        assert_eq!(stop, Stop::Shutdown);
        assert_eq!(calls.get(), 0);
    }
}
