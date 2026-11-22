//! The connector's log subscriber, with a level that can change while it
//! runs (the log level is a runtime setting): the counterpart of the Go
//! `connectors/shared/logging` package. `WUPHF_LOG_LEVEL` overrides the
//! configured level, on every change too, without editing the file; it may be
//! a full `EnvFilter` directive (`whatsapp_rust=debug,info`).

use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Registry, fmt, reload};

pub const ENV_LEVEL: &str = "WUPHF_LOG_LEVEL";

/// The levels a configuration may name (the runtime settings declare the
/// same list).
pub const LEVELS: [&str; 5] = ["trace", "debug", "info", "warn", "error"];

const DEFAULT_LEVEL: &str = "info";

/// An error for a configured level other than the five (any case); empty
/// means info. Connectors check it when loading their file, because the core
/// refuses a settings declaration whose level it does not know (Go
/// `CheckLevel`).
pub fn check_level(configured: &str) -> Result<(), String> {
    if configured.is_empty() || known(configured).is_some() {
        return Ok(());
    }
    Err(format!(
        "log_level must be one of {}, not {configured:?}",
        LEVELS.join(", ")
    ))
}

/// `level` lower-cased, when it is one of the five.
fn known(level: &str) -> Option<String> {
    let level = level.to_lowercase();
    LEVELS.contains(&level.as_str()).then_some(level)
}

/// The filter in force: the environment's directive when it parses, else the
/// configured level when it is one of the five, else info.
pub fn effective_filter(env: Option<&str>, configured: &str) -> EnvFilter {
    if let Some(filter) = env
        .filter(|directive| !directive.is_empty())
        .and_then(|directive| EnvFilter::try_new(directive).ok())
    {
        return filter;
    }
    EnvFilter::new(known(configured).as_deref().unwrap_or(DEFAULT_LEVEL))
}

fn filter_for(configured: &str) -> EnvFilter {
    effective_filter(std::env::var(ENV_LEVEL).ok().as_deref(), configured)
}

/// Changes the level of the installed subscriber. Cheap to clone.
#[derive(Clone)]
pub struct LogLevel {
    handle: reload::Handle<EnvFilter, Registry>,
}

/// Install the global fmt subscriber, with a filter [`LogLevel::set`] can
/// change.
pub fn init(configured: &str) -> LogLevel {
    let (filter, handle) = reload::Layer::new(filter_for(configured));
    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer())
        .init();
    LogLevel { handle }
}

impl LogLevel {
    /// Make `configured` the level, the environment still winning.
    pub fn set(&self, configured: &str) {
        if let Err(error) = self.handle.reload(filter_for(configured)) {
            tracing::warn!(%error, "could not change the log level");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filter(env: Option<&str>, configured: &str) -> String {
        effective_filter(env, configured).to_string()
    }

    #[test]
    fn the_environment_wins_then_the_config_then_info() {
        assert_eq!(filter(Some("debug"), "warn"), "debug");
        assert_eq!(filter(None, "warn"), "warn");
        assert_eq!(filter(Some(""), "WARN"), "warn");
        assert_eq!(filter(None, "verbose"), "info");
        assert_eq!(filter(None, ""), "info");
        assert_eq!(
            filter(Some("whatsapp_rust=debug,info"), "warn"),
            "whatsapp_rust=debug,info"
        );
    }

    #[test]
    fn only_the_five_levels_are_accepted() {
        assert_eq!(check_level("INFO"), Ok(()));
        assert_eq!(check_level(""), Ok(()));
        let error = check_level("verbose").unwrap_err();
        assert!(error.contains("trace, debug, info, warn, error"), "{error}");
    }
}
