//! Adapts wuphf's settings table to botconf's storage contract.

use botconf::{StorageError, StoredOverride};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use wuphf_storage::{SettingsOverrides, StoredSetting};

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// botconf storage over any wuphf backend that keeps settings overrides.
pub struct SettingsStorage<S> {
    backend: Arc<S>,
    now_ms: fn() -> i64,
}

impl<S: SettingsOverrides> SettingsStorage<S> {
    pub fn new(backend: Arc<S>) -> Self {
        Self {
            backend,
            now_ms: system_now_ms,
        }
    }
}

fn system_now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
        })
}

impl<S: SettingsOverrides> botconf::Storage for SettingsStorage<S> {
    fn load(&self) -> BoxFuture<'_, Result<Vec<StoredOverride>, StorageError>> {
        Box::pin(async move {
            let stored = self
                .backend
                .settings_overrides()
                .await
                .map_err(|error| Box::new(error) as StorageError)?;
            Ok(stored
                .into_iter()
                .map(|setting| StoredOverride {
                    key: setting.key,
                    value: setting.value,
                    // botconf's actor is still an integer, and only used when
                    // writing; a non-numeric actor reads back as unknown.
                    by: setting
                        .updated_by
                        .and_then(|actor| actor.parse::<i64>().ok()),
                })
                .collect())
        })
    }

    fn write<'a>(
        &'a self,
        deleted: &'a [String],
        upserted: Option<&'a StoredOverride>,
    ) -> BoxFuture<'a, Result<(), StorageError>> {
        Box::pin(async move {
            let upserted = upserted.map(|stored| StoredSetting {
                key: stored.key.clone(),
                value: stored.value.clone(),
                updated_by: stored.by.map(|by| by.to_string()),
            });
            self.backend
                .write_settings_overrides(deleted, upserted.as_ref(), (self.now_ms)())
                .await
                .map_err(|error| Box::new(error) as StorageError)
        })
    }
}
