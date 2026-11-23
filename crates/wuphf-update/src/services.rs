//! Restarting what runs the updated binaries: systemd units, behind
//! [`Services`] so tests record instead.

use async_trait::async_trait;

use crate::Error;

#[async_trait]
pub trait Services: Send + Sync {
    /// Whether `unit` is running.
    async fn is_active(&self, unit: &str) -> bool;
    async fn restart(&self, unit: &str) -> Result<(), Error>;
}

/// [`Services`] through `systemctl`.
#[derive(Debug, Clone, Copy, Default)]
pub struct Systemd;

#[async_trait]
impl Services for Systemd {
    async fn is_active(&self, unit: &str) -> bool {
        tokio::process::Command::new("systemctl")
            .args(["is-active", "--quiet", "--", unit])
            .status()
            .await
            .is_ok_and(|status| status.success())
    }

    async fn restart(&self, unit: &str) -> Result<(), Error> {
        let output = tokio::process::Command::new("systemctl")
            .args(["restart", "--", unit])
            .output()
            .await
            .map_err(|error| Error::Restart(format!("run systemctl: {error}")))?;
        if output.status.success() {
            Ok(())
        } else {
            Err(Error::Restart(format!(
                "systemctl restart {unit}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )))
        }
    }
}
