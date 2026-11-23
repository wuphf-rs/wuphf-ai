//! The lock a connector holds on its session while it runs, so its setup's
//! actions (linking the account, fetching its chats) never use the session at
//! the same time, and two connectors never share one. The Go connectors'
//! `sessionlock` package is the same lock.
//!
//! The lock is an advisory lock on `<session>.lock`, released when the process
//! exits however it exits; the file itself is left.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};

/// A held session lock, released when dropped.
#[derive(Debug)]
pub struct SessionLock {
    _file: File,
}

#[derive(Debug, thiserror::Error)]
pub enum SessionLockError {
    /// Another process holds it.
    #[error("{0}: the session is in use")]
    Held(PathBuf),
    #[error("lock {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

/// The lock file of `session`.
pub fn lock_path(session: &Path) -> PathBuf {
    let mut path = session.as_os_str().to_owned();
    path.push(".lock");
    path.into()
}

impl SessionLock {
    /// Take `session`'s lock without waiting.
    pub fn acquire(session: &Path) -> Result<Self, SessionLockError> {
        let path = lock_path(session);
        let io = |source| SessionLockError::Io {
            path: path.clone(),
            source,
        };
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(io)?;
        match file.try_lock() {
            Ok(()) => Ok(Self { _file: file }),
            Err(TryLockError::WouldBlock) => Err(SessionLockError::Held(session.to_owned())),
            Err(TryLockError::Error(source)) => Err(io(source)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_is_locked_once() {
        let dir = std::env::temp_dir().join(format!("wuphf-lock-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let session = dir.join("whatsapp.session.db");
        let held = SessionLock::acquire(&session).unwrap();
        // A lock belongs to the open file, so a second open in this process
        // conflicts as another process's would.
        assert!(matches!(
            SessionLock::acquire(&session),
            Err(SessionLockError::Held(_))
        ));
        drop(held);
        SessionLock::acquire(&session).expect("released, it is free again");
        assert_eq!(lock_path(&session), dir.join("whatsapp.session.db.lock"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
