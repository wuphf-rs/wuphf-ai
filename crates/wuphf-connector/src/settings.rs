//! Runtime settings for Rust connectors: the counterpart of the Go
//! `connectors/shared/settings` package (plan 040). A connector keeps the
//! settings that can change while it runs in one serde type, lists them, and
//! declares them when it registers; the core stores and validates every
//! change and pushes it back through its settings service, and the connector
//! reads the live value once per use.
//!
//! Go reflects the declaration from struct tags; Rust has no reflection, so a
//! connector lists its settings ([`RuntimeSettings::settings`]) and the
//! descriptor, the JSON Schema and the base are all derived from that list and
//! the base value. Every field of the section must be listed.

use std::collections::BTreeSet;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use botconf_proto as bc;
use botconf_proto::settings_client::SettingsClient;
use prost::Message;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};
use thiserror::Error;
use tokio_util::sync::CancellationToken;
use tonic::metadata::{Ascii, MetadataValue};
use tonic::transport::{Channel, Endpoint};
use tracing::{debug, warn};
use wuphf_proto::connector as pb;

use crate::client::core_uri;
use crate::media::MediaConnectError;

/// Where the core keeps connectors' settings: `connector_settings.<id>`.
pub const NAMESPACE: &str = "connector_settings";

/// The key of a connector's settings section.
pub fn prefix(connector_id: &str) -> String {
    format!("{NAMESPACE}.{connector_id}")
}

/// One runtime setting (Go: a `botconf`-tagged field).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Setting {
    /// Dotted, relative to the section: `rendering.max_message_length`.
    pub key: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub kind: SettingKind,
}

/// How a setting is edited, and its JSON type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SettingKind {
    Text,
    /// One of these values, each its own label.
    Choice(&'static [&'static str]),
    /// Offered as `✅ Yes` / `❌ No`.
    Bool,
    /// An integer within these bounds.
    Number {
        min: i64,
        max: i64,
    },
}

/// A connector's runtime section.
pub trait RuntimeSettings: Serialize + DeserializeOwned + Clone + Send + Sync + 'static {
    /// Every setting of the section, in the order the panel lists them.
    fn settings() -> Vec<Setting>;
}

#[derive(Debug, Error)]
pub enum SettingsError {
    #[error("encode the settings: {0}")]
    Encode(serde_json::Error),
    #[error("the settings must serialize to a JSON object")]
    NotAnObject,
    #[error("settings fields not listed as settings: {}", .0.join(", "))]
    Unlisted(Vec<String>),
    #[error("settings listed but not fields: {}", .0.join(", "))]
    Missing(Vec<String>),
    #[error("decode {key}: {source}")]
    Decode {
        key: String,
        source: serde_json::Error,
    },
    #[error("apply the pushed settings: {0}")]
    Apply(serde_json::Error),
    #[error(transparent)]
    Connect(#[from] MediaConnectError),
}

/// The registration's declaration: a botconf descriptor of the settings, a
/// JSON Schema of the section (draft 2020-12, nested objects by key, no
/// unknown properties), and `base` as the section's values.
pub fn declare<T: RuntimeSettings>(
    base: &T,
) -> Result<pb::ConnectorSettingsDeclaration, SettingsError> {
    let settings = T::settings();
    let values = serde_json::to_value(base).map_err(SettingsError::Encode)?;
    check_listed(&settings, &values)?;
    let descriptor = bc::DescribeResponse {
        settings: settings.iter().map(descriptor).collect(),
        sections: Vec::new(),
    };
    Ok(pb::ConnectorSettingsDeclaration {
        descriptor: descriptor.encode_to_vec(),
        json_schema: schema(&settings).to_string(),
        base_json: values.to_string(),
    })
}

/// Every leaf of the base is a listed setting and every setting a leaf, so
/// the schema (which has no unknown properties) admits the base.
fn check_listed(settings: &[Setting], values: &Value) -> Result<(), SettingsError> {
    let Value::Object(object) = values else {
        return Err(SettingsError::NotAnObject);
    };
    let mut leaves = BTreeSet::new();
    collect_leaves(object, "", &mut leaves);
    let listed: BTreeSet<String> = settings.iter().map(|s| s.key.to_owned()).collect();
    let unlisted: Vec<String> = leaves.difference(&listed).cloned().collect();
    if !unlisted.is_empty() {
        return Err(SettingsError::Unlisted(unlisted));
    }
    let missing: Vec<String> = listed.difference(&leaves).cloned().collect();
    if !missing.is_empty() {
        return Err(SettingsError::Missing(missing));
    }
    Ok(())
}

fn collect_leaves(object: &Map<String, Value>, prefix: &str, leaves: &mut BTreeSet<String>) {
    for (name, value) in object {
        let key = match prefix {
            "" => name.clone(),
            prefix => format!("{prefix}.{name}"),
        };
        match value {
            Value::Object(nested) => collect_leaves(nested, &key, leaves),
            _ => {
                leaves.insert(key);
            }
        }
    }
}

/// A setting's descriptor, as Go's `kindOf` builds it.
fn descriptor(setting: &Setting) -> bc::SettingDescriptor {
    let choices = |pairs: &[(&str, &str)]| bc::OneOfKind {
        choices: pairs
            .iter()
            .map(|(value, label)| bc::Choice {
                value: (*value).to_owned(),
                label: (*label).to_owned(),
            })
            .collect(),
        ..Default::default()
    };
    let kind = match setting.kind {
        SettingKind::Text => bc::kind::Kind::Text(bc::TextKind::default()),
        SettingKind::Choice(values) => bc::kind::Kind::OneOf(choices(
            &values
                .iter()
                .map(|value| (*value, *value))
                .collect::<Vec<_>>(),
        )),
        SettingKind::Bool => {
            bc::kind::Kind::OneOf(choices(&[("true", "✅ Yes"), ("false", "❌ No")]))
        }
        SettingKind::Number { min, max } => bc::kind::Kind::Number(bc::NumberKind {
            min,
            max,
            ..Default::default()
        }),
    };
    bc::SettingDescriptor {
        key: setting.key.to_owned(),
        title: setting.title.to_owned(),
        description: setting.description.to_owned(),
        kind: Some(bc::Kind { kind: Some(kind) }),
        ..Default::default()
    }
}

/// The section's JSON Schema, in the shape the Go connectors' reflector
/// writes (less its Go-package `$id`): an object per key prefix, each closed
/// to unknown properties.
fn schema(settings: &[Setting]) -> Value {
    let mut root = object_schema();
    for setting in settings {
        let path: Vec<&str> = setting.key.split('.').collect();
        let (leaf, parents) = path.split_last().expect("split yields a name");
        let mut object = root.as_object_mut().expect("an object schema");
        for parent in parents {
            object = properties(object)
                .entry(*parent)
                .or_insert_with(object_schema)
                .as_object_mut()
                .expect("a key prefix is an object schema");
        }
        properties(object).insert((*leaf).to_owned(), leaf_schema(&setting.kind));
    }
    root.as_object_mut().expect("an object schema").insert(
        "$schema".into(),
        json!("https://json-schema.org/draft/2020-12/schema"),
    );
    root
}

fn object_schema() -> Value {
    json!({"properties": {}, "additionalProperties": false, "type": "object"})
}

fn properties(object_schema: &mut Map<String, Value>) -> &mut Map<String, Value> {
    object_schema["properties"]
        .as_object_mut()
        .expect("an object schema has properties")
}

fn leaf_schema(kind: &SettingKind) -> Value {
    match kind {
        SettingKind::Text => json!({"type": "string"}),
        SettingKind::Choice(values) => json!({"type": "string", "enum": values}),
        SettingKind::Bool => json!({"type": "boolean"}),
        SettingKind::Number { .. } => json!({"type": "integer"}),
    }
}

/// The current value; readers load it once per use, so one use sees one
/// consistent value.
pub struct Live<T> {
    value: RwLock<Arc<T>>,
}

impl<T> Live<T> {
    pub fn new(initial: T) -> Self {
        Self {
            value: RwLock::new(Arc::new(initial)),
        }
    }

    pub fn load(&self) -> Arc<T> {
        Arc::clone(&self.value.read().unwrap_or_else(|e| e.into_inner()))
    }

    /// Replace the value, returning the previous one.
    pub fn set(&self, value: T) -> Arc<T> {
        self.swap(Arc::new(value))
    }

    fn swap(&self, value: Arc<T>) -> Arc<T> {
        let mut current = self.value.write().unwrap_or_else(|e| e.into_inner());
        std::mem::replace(&mut *current, value)
    }
}

/// Lays one watch event over `current` (an initial event, which carries the
/// whole section, over `base`): values without JSON and keys outside `prefix`
/// are ignored; whole floats become integers (Go `integers`), so a number the
/// core stored as `20000.0` still fills an integer field.
pub fn apply<T: RuntimeSettings>(
    current: &T,
    base: &T,
    event: &bc::WatchEvent,
    prefix: &str,
) -> Result<T, SettingsError> {
    let start = if event.initial { base } else { current };
    let mut pushed = Map::new();
    for entry in &event.values {
        let Some(json) = &entry.value_json else {
            continue;
        };
        let decoded: Value =
            serde_json::from_str(json).map_err(|source| SettingsError::Decode {
                key: entry.key.clone(),
                source,
            })?;
        if entry.key == prefix {
            if let Value::Object(object) = decoded {
                merge(&mut pushed, object);
            }
            continue;
        }
        let Some(path) = entry
            .key
            .strip_prefix(prefix)
            .and_then(|rest| rest.strip_prefix('.'))
        else {
            continue;
        };
        place(&mut pushed, path, decoded);
    }
    if pushed.is_empty() {
        return Ok(start.clone());
    }
    let Value::Object(mut merged) = serde_json::to_value(start).map_err(SettingsError::Encode)?
    else {
        return Err(SettingsError::NotAnObject);
    };
    let Value::Object(pushed) = integers(Value::Object(pushed)) else {
        unreachable!("integers keeps an object an object");
    };
    merge(&mut merged, pushed);
    serde_json::from_value(Value::Object(merged)).map_err(SettingsError::Apply)
}

/// Sets the dotted `path` in `object`, creating the objects on the way.
fn place(object: &mut Map<String, Value>, path: &str, value: Value) {
    let mut segments: Vec<&str> = path.split('.').collect();
    let leaf = segments.pop().expect("split yields one segment at least");
    let mut object = object;
    for segment in segments {
        let next = object
            .entry(segment)
            .or_insert_with(|| Value::Object(Map::new()));
        if !next.is_object() {
            *next = Value::Object(Map::new());
        }
        object = next.as_object_mut().expect("just made an object");
    }
    object.insert(leaf.to_owned(), value);
}

/// Lays `incoming` over `base`, object by object.
fn merge(base: &mut Map<String, Value>, incoming: Map<String, Value>) {
    for (key, value) in incoming {
        match (base.get_mut(&key), value) {
            (Some(Value::Object(existing)), Value::Object(nested)) => merge(existing, nested),
            (_, value) => {
                base.insert(key, value);
            }
        }
    }
}

/// JSON's whole floats back as integers.
fn integers(value: Value) -> Value {
    match value {
        Value::Object(object) => Value::Object(
            object
                .into_iter()
                .map(|(key, value)| (key, integers(value)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.into_iter().map(integers).collect()),
        Value::Number(number) => match number.as_f64() {
            Some(float)
                if !number.is_i64()
                    && !number.is_u64()
                    && float.fract() == 0.0
                    && float.abs() < (1u64 << 53) as f64 =>
            {
                json!(float as i64)
            }
            _ => Value::Number(number),
        },
        other => other,
    }
}

/// How long to wait before re-opening the watch stream: `initial`, doubling
/// to `max`, back to `initial` once a stream delivered an event.
#[derive(Clone, Copy, Debug)]
pub struct Backoff {
    pub initial: Duration,
    pub max: Duration,
}

impl Default for Backoff {
    fn default() -> Self {
        Self {
            initial: Duration::from_secs(1),
            max: Duration::from_secs(30),
        }
    }
}

/// The declaration, the live value, and a client of the core's settings
/// service (Go `settings.Connection`).
pub struct Connection<T> {
    /// Goes into the registration ([`crate::Options::settings`]).
    pub declaration: pb::ConnectorSettingsDeclaration,
    /// The current value; read it once per use.
    pub live: Arc<Live<T>>,
    base: T,
    prefix: String,
    client: SettingsClient<Channel>,
    authorization: MetadataValue<Ascii>,
    backoff: Backoff,
}

impl<T: RuntimeSettings> Connection<T> {
    /// Declare `base` (the connector's own file values, defaults applied) and
    /// set up a lazy connection to the core's settings service with the
    /// connector's core token.
    pub fn connect(
        core_address: &str,
        token: &str,
        connector_id: &str,
        base: T,
    ) -> Result<Self, SettingsError> {
        let declaration = declare(&base)?;
        let channel = Endpoint::new(core_uri(core_address).map_err(MediaConnectError::from)?)
            .map_err(MediaConnectError::from)?
            .connect_lazy();
        let authorization = format!("Bearer {token}")
            .parse()
            .map_err(MediaConnectError::from)?;
        Ok(Self {
            declaration,
            live: Arc::new(Live::new(base.clone())),
            base,
            prefix: prefix(connector_id),
            client: SettingsClient::new(channel),
            authorization,
            backoff: Backoff::default(),
        })
    }

    /// The reconnect delays (tests shorten them).
    pub fn with_backoff(mut self, backoff: Backoff) -> Self {
        self.backoff = backoff;
        self
    }

    /// Apply the core's pushes until `cancel` fires. After each (re)connect
    /// the core sends the whole section; the stream is re-opened with
    /// [`Backoff`] when it ends. `on_change(old, new)` runs after each swap. A
    /// push that does not decode is logged at warn and skipped: the connector
    /// keeps its last value.
    pub async fn follow(
        &self,
        cancel: CancellationToken,
        on_change: impl Fn(&T, &T) + Send + Sync,
    ) {
        let mut delay = self.backoff.initial;
        loop {
            let (received, ended) = tokio::select! {
                () = cancel.cancelled() => return,
                outcome = self.watch_once(&on_change) => outcome,
            };
            if received {
                delay = self.backoff.initial;
            }
            debug!(reason = %ended, "settings stream interrupted; reconnecting");
            tokio::select! {
                () = cancel.cancelled() => return,
                () = tokio::time::sleep(delay) => {}
            }
            delay = (delay * 2).min(self.backoff.max);
        }
    }

    /// One watch stream until it ends: whether any event arrived, and why it
    /// ended.
    async fn watch_once(&self, on_change: &(impl Fn(&T, &T) + Send + Sync)) -> (bool, String) {
        let mut request = tonic::Request::new(bc::WatchRequest {
            actor: None,
            prefixes: vec![self.prefix.clone()],
        });
        request
            .metadata_mut()
            .insert("authorization", self.authorization.clone());
        let mut stream = match self.client.clone().watch(request).await {
            Ok(response) => response.into_inner(),
            Err(status) => return (false, status.to_string()),
        };
        let mut received = false;
        loop {
            match stream.message().await {
                Ok(Some(event)) => {
                    received = true;
                    self.handle(&event, on_change);
                }
                Ok(None) => return (received, "the core ended the stream".into()),
                Err(status) => return (received, status.to_string()),
            }
        }
    }

    fn handle(&self, event: &bc::WatchEvent, on_change: &impl Fn(&T, &T)) {
        let current = self.live.load();
        match apply(&*current, &self.base, event, &self.prefix) {
            Ok(next) => {
                let next = Arc::new(next);
                let old = self.live.swap(Arc::clone(&next));
                on_change(&old, &next);
            }
            Err(error) => warn!(%error, "ignoring a runtime settings update"),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use super::*;

    #[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
    struct Section {
        level: String,
        verbose: bool,
        limit: u32,
        nested: Nested,
    }

    #[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
    struct Nested {
        name: String,
    }

    impl RuntimeSettings for Section {
        fn settings() -> Vec<Setting> {
            vec![
                Setting {
                    key: "level",
                    title: "Level",
                    description: "How loud",
                    kind: SettingKind::Choice(&["low", "high"]),
                },
                Setting {
                    key: "verbose",
                    title: "Verbose",
                    description: "Say more",
                    kind: SettingKind::Bool,
                },
                Setting {
                    key: "limit",
                    title: "Limit",
                    description: "At most",
                    kind: SettingKind::Number { min: 0, max: 10 },
                },
                Setting {
                    key: "nested.name",
                    title: "Name",
                    description: "Called",
                    kind: SettingKind::Text,
                },
            ]
        }
    }

    const PREFIX: &str = "connector_settings.test";

    fn base() -> Section {
        Section {
            level: "low".into(),
            limit: 3,
            nested: Nested { name: "a".into() },
            ..Section::default()
        }
    }

    fn entry(key: &str, json: Option<&str>) -> bc::ValueEntry {
        bc::ValueEntry {
            key: key.into(),
            value_json: json.map(str::to_owned),
            ..Default::default()
        }
    }

    fn event(initial: bool, values: Vec<bc::ValueEntry>) -> bc::WatchEvent {
        bc::WatchEvent {
            values,
            initial,
            ..Default::default()
        }
    }

    #[test]
    fn declare_lists_the_settings_with_their_kinds() {
        let declaration = declare(&base()).unwrap();
        let described = bc::DescribeResponse::decode(declaration.descriptor.as_slice()).unwrap();
        let keys: Vec<&str> = described.settings.iter().map(|s| s.key.as_str()).collect();
        assert_eq!(keys, ["level", "verbose", "limit", "nested.name"]);
        let kind = |index: usize| {
            described.settings[index]
                .kind
                .clone()
                .unwrap()
                .kind
                .unwrap()
        };
        let labels = |kind: bc::kind::Kind| match kind {
            bc::kind::Kind::OneOf(one_of) => one_of
                .choices
                .into_iter()
                .map(|choice| (choice.value, choice.label))
                .collect::<Vec<_>>(),
            other => panic!("a choice, not {other:?}"),
        };
        assert_eq!(
            labels(kind(0)),
            [("low".into(), "low".into()), ("high".into(), "high".into())]
        );
        assert_eq!(
            labels(kind(1)),
            [
                ("true".into(), "✅ Yes".into()),
                ("false".into(), "❌ No".into())
            ]
        );
        assert_eq!(
            kind(2),
            bc::kind::Kind::Number(bc::NumberKind {
                min: 0,
                max: 10,
                ..Default::default()
            })
        );
        assert_eq!(kind(3), bc::kind::Kind::Text(bc::TextKind::default()));
        assert_eq!(described.settings[3].title, "Name");
        assert_eq!(described.settings[3].description, "Called");
        assert_eq!(described.settings[3].position, 0);
    }

    #[test]
    fn the_schema_nests_dotted_keys() {
        let declaration = declare(&base()).unwrap();
        let schema: Value = serde_json::from_str(&declaration.json_schema).unwrap();
        assert_eq!(
            schema,
            json!({
                "$schema": "https://json-schema.org/draft/2020-12/schema",
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "level": {"type": "string", "enum": ["low", "high"]},
                    "verbose": {"type": "boolean"},
                    "limit": {"type": "integer"},
                    "nested": {
                        "type": "object",
                        "additionalProperties": false,
                        "properties": {"name": {"type": "string"}}
                    }
                }
            })
        );
    }

    #[test]
    fn the_base_is_the_values() {
        let declaration = declare(&base()).unwrap();
        let values: Value = serde_json::from_str(&declaration.base_json).unwrap();
        assert_eq!(values, serde_json::to_value(base()).unwrap());
    }

    #[test]
    fn every_field_must_be_listed() {
        #[derive(Clone, Default, Serialize, Deserialize)]
        struct Extra {
            listed: bool,
            forgotten: bool,
        }
        impl RuntimeSettings for Extra {
            fn settings() -> Vec<Setting> {
                vec![Setting {
                    key: "listed",
                    title: "Listed",
                    description: "",
                    kind: SettingKind::Bool,
                }]
            }
        }
        let error = declare(&Extra::default()).unwrap_err();
        assert!(
            matches!(&error, SettingsError::Unlisted(keys) if keys == &["forgotten"]),
            "{error}"
        );
    }

    #[test]
    fn a_first_event_starts_from_the_base() {
        let current = Section {
            verbose: true,
            limit: 9,
            ..base()
        };
        let next = apply(
            &current,
            &base(),
            &event(true, vec![entry(&format!("{PREFIX}.limit"), Some("5"))]),
            PREFIX,
        )
        .unwrap();
        assert_eq!(next, Section { limit: 5, ..base() });
    }

    #[test]
    fn later_events_change_only_their_keys() {
        let current = Section {
            verbose: true,
            ..base()
        };
        let next = apply(
            &current,
            &base(),
            &event(
                false,
                vec![entry(&format!("{PREFIX}.nested.name"), Some("\"b\""))],
            ),
            PREFIX,
        )
        .unwrap();
        assert_eq!(
            next,
            Section {
                nested: Nested { name: "b".into() },
                ..current
            }
        );
    }

    #[test]
    fn whole_floats_decode_into_integers() {
        let next = apply(
            &base(),
            &base(),
            &event(false, vec![entry(&format!("{PREFIX}.limit"), Some("8.0"))]),
            PREFIX,
        )
        .unwrap();
        assert_eq!(next.limit, 8);
    }

    #[test]
    fn an_undecodable_push_is_refused() {
        for json in ["\"many\"", "not json"] {
            let result = apply(
                &base(),
                &base(),
                &event(false, vec![entry(&format!("{PREFIX}.limit"), Some(json))]),
                PREFIX,
            );
            assert!(result.is_err(), "{json}");
        }
    }

    #[test]
    fn values_without_json_or_outside_the_prefix_are_ignored() {
        let next = apply(
            &base(),
            &base(),
            &event(
                false,
                vec![
                    entry(&format!("{PREFIX}.limit"), None),
                    entry("connector_settings.other.limit", Some("7")),
                    entry(&format!("{PREFIX}x.limit"), Some("7")),
                ],
            ),
            PREFIX,
        )
        .unwrap();
        assert_eq!(next, base());
        // The section itself, as one object, is laid over the value.
        let next = apply(
            &base(),
            &base(),
            &event(false, vec![entry(PREFIX, Some(r#"{"verbose":true}"#))]),
            PREFIX,
        )
        .unwrap();
        assert!(next.verbose);
    }

    #[test]
    fn live_swaps_whole_values() {
        let live = Arc::new(Live::new((0u64, 0u64)));
        let old = live.set((1, 1));
        assert_eq!(*old, (0, 0));
        let writer = {
            let live = Arc::clone(&live);
            std::thread::spawn(move || {
                for n in 2..2000 {
                    live.set((n, n));
                }
            })
        };
        for _ in 0..2000 {
            let (a, b) = *live.load();
            assert_eq!(a, b);
        }
        writer.join().unwrap();
        assert_eq!(*live.load(), (1999, 1999));
    }
}
