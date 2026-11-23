//! The `[functions]` configuration section and its resolved form. The raw
//! [`FunctionsConfig`] is what TOML deserializes into; [`FunctionsConfig::resolve`]
//! validates it, reads tokens from the environment and produces [`Functions`],
//! which the hub and services consult for authorization, deadlines and limits.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::time::Duration;
use thiserror::Error;
use wuphf_proto::connector as pb;

/// Remote functions (see docs/connector-protocol.md, "Remote functions").
/// Providers dial the core and serve named functions; connectors call them.
/// A call to a name no provider below may serve fails at once; a call to a
/// servable name waits for a provider until its deadline. All durations are
/// milliseconds.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[cfg_attr(feature = "describe", derive(botconf::Describe))]
#[serde(default)]
pub struct FunctionsConfig {
    /// Deadline for a call whose caller sets none.
    pub default_deadline_ms: u64,
    /// Cap on any call's deadline, including the caller's own.
    pub max_deadline_ms: u64,
    /// HTTP/2 keepalive ping interval, server-wide.
    pub keepalive_interval_ms: u64,
    /// How long a keepalive ping may go unanswered before the peer is dropped.
    pub keepalive_timeout_ms: u64,
    /// Cap on a call's JSON args and a provider's JSON result.
    pub max_body_bytes: usize,
    /// Calls that may wait for a provider, per function name and major.
    pub max_queued_calls: usize,
    /// Environment variable holding the shared token callers present to
    /// FunctionTransport (connectors: their `functions_token_env`). Unset: no
    /// token is asked for.
    #[cfg_attr(feature = "describe", botconf(env = token, hint = "WUPHF_INVOKE_TOKEN"))]
    pub invoke_token_env: Option<String>,
    /// The provider processes, and which function names each may serve.
    ///
    /// The sticker converter is one: configure it from
    /// providers/mediaconv/config.example.toml. The Instagram connector can
    /// serve `instagram.fetch_media@1` (wuphf-misc uses it): enable its
    /// `[functions]` section with the same id and token variable, and list it
    /// here (`id = "instagram-main"`, `token_env =
    /// "WUPHF_PROVIDER_INSTAGRAM_TOKEN"`, `functions = ["instagram.fetch_media"]`).
    #[cfg_attr(
        feature = "describe",
        botconf(example = [{
            "id": "mediaconv",
            "token_env": "WUPHF_PROVIDER_MEDIACONV_TOKEN",
            "functions": ["media.convert"],
        }])
    )]
    pub providers: Vec<ProviderConfig>,
    /// Limits for one function name (every major), replacing the global ones:
    /// `[functions.overrides."media.convert"]`.
    pub overrides: BTreeMap<String, FunctionOverride>,
}

impl Default for FunctionsConfig {
    fn default() -> Self {
        Self {
            default_deadline_ms: 30_000,
            max_deadline_ms: 60_000,
            keepalive_interval_ms: 10_000,
            keepalive_timeout_ms: 20_000,
            max_body_bytes: 1_048_576,
            max_queued_calls: 256,
            invoke_token_env: None,
            providers: Vec::new(),
            overrides: BTreeMap::new(),
        }
    }
}

/// A provider process allowed to serve functions.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[cfg_attr(feature = "describe", derive(botconf::Describe))]
pub struct ProviderConfig {
    /// The id the provider registers with.
    pub id: String,
    /// Environment variable holding this provider's token (the provider's
    /// own config names the same variable).
    #[cfg_attr(feature = "describe", botconf(env = token))]
    pub token_env: String,
    /// Names this provider may register: exact names, `prefix.*`, or `*`.
    pub functions: Vec<String>,
}

/// Per-function limits that replace the global ones.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[cfg_attr(feature = "describe", derive(botconf::Describe))]
#[serde(default)]
pub struct FunctionOverride {
    /// Replaces `default_deadline_ms`.
    pub default_deadline_ms: Option<u64>,
    /// Replaces `max_deadline_ms`.
    pub max_deadline_ms: Option<u64>,
    /// Caps the calls one provider runs at once, below what it declares.
    pub max_concurrency: Option<u32>,
    /// Replaces `max_body_bytes`.
    pub max_body_bytes: Option<usize>,
    /// Replaces `max_queued_calls`.
    pub max_queued_calls: Option<usize>,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ConfigError {
    #[error("{field} must be greater than zero")]
    Zero { field: String },
    #[error("{scope}: default_deadline_ms ({default}) exceeds max_deadline_ms ({max})")]
    DefaultAboveMax {
        scope: String,
        default: u64,
        max: u64,
    },
    #[error("a function provider has an empty id")]
    EmptyProviderId,
    #[error("function provider {0} is configured more than once")]
    DuplicateProviderId(String),
    #[error("function provider {0} lists no functions")]
    NoFunctions(String),
    #[error(
        "function provider {provider} has invalid pattern {pattern:?}; use `*`, a dotted name, or a dotted name followed by `.*`"
    )]
    InvalidPattern { provider: String, pattern: String },
    #[error("functions.overrides key {0:?} is not a valid function name")]
    InvalidOverride(String),
    #[error("{owner} reads its token from {variable}, which is unset or empty")]
    MissingToken { owner: String, variable: String },
}

/// A function's identity: its dotted name and major version.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FunctionKey {
    pub name: String,
    pub major: u32,
}

impl FunctionKey {
    pub(crate) fn from_proto(function: Option<pb::FunctionRef>) -> Result<Self, String> {
        let function = function.ok_or("function reference required")?;
        if !is_valid_name(&function.name) {
            return Err(format!(
                "function name {:?} must be lowercase dotted segments, e.g. media.convert",
                function.name
            ));
        }
        if function.major == 0 {
            return Err(format!(
                "{} needs a major version of at least 1",
                function.name
            ));
        }
        Ok(Self {
            name: function.name,
            major: function.major,
        })
    }

    pub(crate) fn to_proto(&self) -> pb::FunctionRef {
        pb::FunctionRef {
            name: self.name.clone(),
            major: self.major,
        }
    }
}

impl fmt::Display for FunctionKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}@{}", self.name, self.major)
    }
}

/// The resolved, validated `[functions]` section.
pub struct Functions {
    default_deadline: Duration,
    max_deadline: Duration,
    keepalive_interval: Duration,
    keepalive_timeout: Duration,
    max_body_bytes: usize,
    max_queued_calls: usize,
    invoke_token: Option<String>,
    providers: Vec<Provider>,
    overrides: BTreeMap<String, Override>,
}

struct Provider {
    id: String,
    token: String,
    patterns: Vec<Pattern>,
}

enum Pattern {
    Any,
    /// Matches names starting with this prefix, which ends in a dot.
    Prefix(String),
    Exact(String),
}

impl Pattern {
    fn parse(pattern: &str) -> Option<Self> {
        if pattern == "*" {
            return Some(Self::Any);
        }
        if let Some(prefix) = pattern.strip_suffix(".*") {
            return prefix
                .split('.')
                .all(is_valid_segment)
                .then(|| Self::Prefix(format!("{prefix}.")));
        }
        is_valid_name(pattern).then(|| Self::Exact(pattern.to_owned()))
    }

    fn matches(&self, name: &str) -> bool {
        match self {
            Self::Any => true,
            Self::Prefix(prefix) => name.starts_with(prefix.as_str()),
            Self::Exact(exact) => name == exact,
        }
    }
}

struct Override {
    default_deadline: Option<Duration>,
    max_deadline: Option<Duration>,
    max_concurrency: Option<u32>,
    max_body_bytes: Option<usize>,
    max_queued_calls: Option<usize>,
}

impl FunctionsConfig {
    /// Validate the section and read every token from `env`.
    pub fn resolve(self, env: impl Fn(&str) -> Option<String>) -> Result<Functions, ConfigError> {
        for (field, value) in [
            ("functions.default_deadline_ms", self.default_deadline_ms),
            ("functions.max_deadline_ms", self.max_deadline_ms),
            (
                "functions.keepalive_interval_ms",
                self.keepalive_interval_ms,
            ),
            ("functions.keepalive_timeout_ms", self.keepalive_timeout_ms),
            ("functions.max_body_bytes", self.max_body_bytes as u64),
            ("functions.max_queued_calls", self.max_queued_calls as u64),
        ] {
            require_positive(field, value)?;
        }
        if self.default_deadline_ms > self.max_deadline_ms {
            return Err(ConfigError::DefaultAboveMax {
                scope: "functions".into(),
                default: self.default_deadline_ms,
                max: self.max_deadline_ms,
            });
        }

        let token = |owner: String, variable: &str| {
            env(variable)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| ConfigError::MissingToken {
                    owner,
                    variable: variable.to_owned(),
                })
        };
        let invoke_token = self
            .invoke_token_env
            .as_deref()
            .map(|variable| token("functions.invoke_token_env".into(), variable))
            .transpose()?;

        let mut seen = HashSet::new();
        let mut providers = Vec::with_capacity(self.providers.len());
        for provider in self.providers {
            if provider.id.is_empty() {
                return Err(ConfigError::EmptyProviderId);
            }
            if !seen.insert(provider.id.clone()) {
                return Err(ConfigError::DuplicateProviderId(provider.id));
            }
            if provider.functions.is_empty() {
                return Err(ConfigError::NoFunctions(provider.id));
            }
            let patterns = provider
                .functions
                .iter()
                .map(|pattern| {
                    Pattern::parse(pattern).ok_or_else(|| ConfigError::InvalidPattern {
                        provider: provider.id.clone(),
                        pattern: pattern.clone(),
                    })
                })
                .collect::<Result<_, _>>()?;
            let token = token(
                format!("function provider {}", provider.id),
                &provider.token_env,
            )?;
            providers.push(Provider {
                id: provider.id,
                token,
                patterns,
            });
        }

        let mut overrides = BTreeMap::new();
        for (name, entry) in self.overrides {
            if !is_valid_name(&name) {
                return Err(ConfigError::InvalidOverride(name));
            }
            let scope = format!("functions.overrides.\"{name}\"");
            for (field, value) in [
                ("default_deadline_ms", entry.default_deadline_ms),
                ("max_deadline_ms", entry.max_deadline_ms),
                ("max_concurrency", entry.max_concurrency.map(u64::from)),
                ("max_body_bytes", entry.max_body_bytes.map(|v| v as u64)),
                ("max_queued_calls", entry.max_queued_calls.map(|v| v as u64)),
            ] {
                if let Some(value) = value {
                    require_positive(&format!("{scope}.{field}"), value)?;
                }
            }
            if let Some(default) = entry.default_deadline_ms {
                let max = entry.max_deadline_ms.unwrap_or(self.max_deadline_ms);
                if default > max {
                    return Err(ConfigError::DefaultAboveMax {
                        scope,
                        default,
                        max,
                    });
                }
            }
            overrides.insert(
                name,
                Override {
                    default_deadline: entry.default_deadline_ms.map(Duration::from_millis),
                    max_deadline: entry.max_deadline_ms.map(Duration::from_millis),
                    max_concurrency: entry.max_concurrency,
                    max_body_bytes: entry.max_body_bytes,
                    max_queued_calls: entry.max_queued_calls,
                },
            );
        }

        Ok(Functions {
            default_deadline: Duration::from_millis(self.default_deadline_ms),
            max_deadline: Duration::from_millis(self.max_deadline_ms),
            keepalive_interval: Duration::from_millis(self.keepalive_interval_ms),
            keepalive_timeout: Duration::from_millis(self.keepalive_timeout_ms),
            max_body_bytes: self.max_body_bytes,
            max_queued_calls: self.max_queued_calls,
            invoke_token,
            providers,
            overrides,
        })
    }
}

impl Functions {
    /// The shared token callers must present, if one is configured.
    pub fn invoke_token(&self) -> Option<&str> {
        self.invoke_token.as_deref()
    }

    /// Every provider's token, for services providers also use, such as media.
    pub fn provider_tokens(&self) -> impl Iterator<Item = &str> {
        self.providers
            .iter()
            .map(|provider| provider.token.as_str())
    }

    pub fn keepalive_interval(&self) -> Duration {
        self.keepalive_interval
    }

    pub fn keepalive_timeout(&self) -> Duration {
        self.keepalive_timeout
    }

    /// True only for a configured provider presenting its own token.
    pub(crate) fn authenticate(&self, provider_id: &str, token: Option<&str>) -> bool {
        self.provider(provider_id)
            .is_some_and(|provider| token == Some(provider.token.as_str()))
    }

    pub(crate) fn may_register(&self, provider_id: &str, name: &str) -> bool {
        self.provider(provider_id).is_some_and(|provider| {
            provider
                .patterns
                .iter()
                .any(|pattern| pattern.matches(name))
        })
    }

    /// Whether some configured provider may register `name`. A call to a name
    /// none may serve fails at once instead of waiting.
    pub(crate) fn is_servable(&self, name: &str) -> bool {
        self.providers.iter().any(|provider| {
            provider
                .patterns
                .iter()
                .any(|pattern| pattern.matches(name))
        })
    }

    /// The deadline for one call: what the caller requested, else the
    /// configured default for the function, else the provider's suggestion,
    /// else the global default; always capped by the function's maximum.
    pub(crate) fn deadline(
        &self,
        name: &str,
        requested: Option<Duration>,
        suggested: Option<Duration>,
    ) -> Duration {
        let entry = self.overrides.get(name);
        let max = entry
            .and_then(|entry| entry.max_deadline)
            .unwrap_or(self.max_deadline);
        let default = entry
            .and_then(|entry| entry.default_deadline)
            .or(suggested)
            .unwrap_or(self.default_deadline);
        requested.unwrap_or(default).min(max)
    }

    pub(crate) fn max_concurrency(&self, name: &str, declared: u32) -> u32 {
        let cap = self
            .overrides
            .get(name)
            .and_then(|entry| entry.max_concurrency)
            .unwrap_or(u32::MAX);
        declared.max(1).min(cap)
    }

    pub(crate) fn max_body_bytes(&self, name: &str) -> usize {
        self.overrides
            .get(name)
            .and_then(|entry| entry.max_body_bytes)
            .unwrap_or(self.max_body_bytes)
    }

    pub(crate) fn max_queued_calls(&self, name: &str) -> usize {
        self.overrides
            .get(name)
            .and_then(|entry| entry.max_queued_calls)
            .unwrap_or(self.max_queued_calls)
    }

    fn provider(&self, provider_id: &str) -> Option<&Provider> {
        self.providers
            .iter()
            .find(|provider| provider.id == provider_id)
    }

    /// Resolve `config` with `tokens` (env-var name, value) as the environment.
    #[cfg(test)]
    pub(crate) fn for_tests(config: FunctionsConfig, tokens: &[(&str, &str)]) -> Self {
        config
            .resolve(|variable| {
                tokens
                    .iter()
                    .find(|(name, _)| *name == variable)
                    .map(|(_, value)| (*value).to_owned())
            })
            .unwrap()
    }
}

fn require_positive(field: &str, value: u64) -> Result<(), ConfigError> {
    if value == 0 {
        return Err(ConfigError::Zero {
            field: field.to_owned(),
        });
    }
    Ok(())
}

/// Two or more dot-separated segments, each `[a-z][a-z0-9_]*`.
pub(crate) fn is_valid_name(name: &str) -> bool {
    let mut segments = 0;
    for segment in name.split('.') {
        if !is_valid_segment(segment) {
            return false;
        }
        segments += 1;
    }
    segments >= 2
}

fn is_valid_segment(segment: &str) -> bool {
    let mut characters = segment.chars();
    characters
        .next()
        .is_some_and(|first| first.is_ascii_lowercase())
        && characters.all(|rest| rest.is_ascii_lowercase() || rest.is_ascii_digit() || rest == '_')
}
