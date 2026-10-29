//! Keeps the settings panel's button handles in wuphf's database, so buttons
//! whose action is too long for a platform's button data keep working after
//! the core restarts.

use botconf::StorageError;
use botconf_panel::TokenStore;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use wuphf_storage::PanelTokens;

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// botconf-panel's token store over any wuphf backend that keeps panel
/// tokens.
pub struct PanelTokenStore<S> {
    backend: Arc<S>,
}

impl<S: PanelTokens> PanelTokenStore<S> {
    pub fn new(backend: Arc<S>) -> Self {
        Self { backend }
    }
}

fn storage_error(error: wuphf_storage::StorageError) -> StorageError {
    Box::new(error)
}

impl<S: PanelTokens> TokenStore for PanelTokenStore<S> {
    fn put<'a>(
        &'a self,
        key: &'a str,
        action: &'a str,
        now_ms: i64,
        expires_at_ms: i64,
    ) -> BoxFuture<'a, Result<bool, StorageError>> {
        Box::pin(async move {
            self.backend
                .put_panel_token(key, action, now_ms, expires_at_ms)
                .await
                .map_err(storage_error)
        })
    }

    fn get<'a>(
        &'a self,
        key: &'a str,
        now_ms: i64,
    ) -> BoxFuture<'a, Result<Option<String>, StorageError>> {
        Box::pin(async move {
            self.backend
                .panel_token(key, now_ms)
                .await
                .map_err(storage_error)
        })
    }

    fn purge_expired(&self, now_ms: i64) -> BoxFuture<'_, Result<u64, StorageError>> {
        Box::pin(async move {
            self.backend
                .purge_panel_tokens(now_ms)
                .await
                .map_err(storage_error)
        })
    }
}
