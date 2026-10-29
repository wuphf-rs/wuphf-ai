//! Connectors' own runtime settings (plan 040).
//!
//! A connector declares its runtime settings when it registers: a botconf
//! descriptor of the settings, a JSON Schema of its section and its own file
//! values. The core turns that into a botconf section at
//! `connector_settings.<id>`, so the settings store keeps, validates and
//! publishes the connector's settings like its own, and the connector follows
//! them through the settings service's `Watch`.
//!
//! Only live-safe keys may be declared ([`LIVE_SAFE`]): a runtime setting
//! applies at once, so identity, addresses, session files and capabilities
//! stay in the connector's file. Each declaration is also stored, so the
//! section exists at startup, before the connector reconnects.

use botconf::{
    Choices, DynamicSection, Field, FixedChoice, Form, Kind, RuntimeSetting, Shared, Validator,
};
use botconf_grpc::proto;
use prost::Message;
use serde_json::Value;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use wuphf_model::ConnectorId;
use wuphf_settings::{CONNECTOR_SETTINGS, SettingsStore};
use wuphf_storage::{SettingsDeclaration, SettingsDeclarations};

/// The keys a connector may declare, with everything below them: those that
/// can change while the connector runs.
pub const LIVE_SAFE: &[&str] = &[
    "rendering",
    "sticker_metadata",
    "log_level",
    "media_convert_timeout_ms",
];

/// Where a connector's settings live.
pub fn section_path(connector: &ConnectorId) -> String {
    format!("{CONNECTOR_SETTINGS}.{}", connector.0)
}

/// The id of a connector's section in the settings store.
fn section_id(connector: &ConnectorId) -> String {
    format!("connector:{}", connector.0)
}

/// Registers connectors' settings sections in the core's settings store and
/// remembers their declarations.
pub struct ConnectorSettings {
    store: Arc<SettingsStore>,
    declarations: Arc<dyn SettingsDeclarations>,
}

impl ConnectorSettings {
    pub fn new(store: Arc<SettingsStore>, declarations: Arc<dyn SettingsDeclarations>) -> Self {
        Self {
            store,
            declarations,
        }
    }

    /// Registers the section of every stored declaration, so stored overrides
    /// apply before connectors return. A declaration that no longer fits (its
    /// connector left the configuration, or it no longer validates) is
    /// skipped with a warning; its overrides wait for the connector.
    pub async fn restore(&self) -> Result<(), wuphf_storage::StorageError> {
        for declaration in self.declarations.settings_declarations().await? {
            let connector = declaration.connector_id.clone();
            let registered = match self.section(&declaration) {
                Ok(section) => self
                    .store
                    .register_section(section)
                    .await
                    .map(drop)
                    .map_err(|error| error.to_string()),
                Err(error) => Err(error),
            };
            if let Err(error) = registered {
                tracing::warn!(
                    connector = %connector.0,
                    %error,
                    "skipping a stored connector settings declaration",
                );
            }
        }
        Ok(())
    }

    /// The section a declaration describes, or why it is refused. Checked
    /// before a connector's session is accepted.
    pub fn section(&self, declaration: &SettingsDeclaration) -> Result<DynamicSection, String> {
        let connector = &declaration.connector_id;
        let snapshot = self.store.current();
        let platform = snapshot
            .config
            .connectors
            .iter()
            .find(|configured| configured.id == connector.0)
            .map(|configured| configured.platform.clone())
            .ok_or_else(|| format!("connector {} is not configured", connector.0))?;
        section_for(connector, &platform, declaration)
    }

    /// Stores a connector's declaration and registers (or replaces) its
    /// section. Call [`Self::section`] first to refuse a bad declaration
    /// before the session is accepted.
    pub async fn declare(&self, declaration: SettingsDeclaration) -> Result<(), String> {
        let section = self.section(&declaration)?;
        self.declarations
            .put_settings_declaration(&declaration, now_ms())
            .await
            .map_err(|error| error.to_string())?;
        self.store
            .register_section(section)
            .await
            .map(drop)
            .map_err(|error| error.to_string())
    }
}

/// The botconf section a connector's declaration describes: its settings
/// (live-safe keys only), validated by its JSON Schema, over its own values.
pub fn section_for(
    connector: &ConnectorId,
    platform: &str,
    declaration: &SettingsDeclaration,
) -> Result<DynamicSection, String> {
    let described = proto::DescribeResponse::decode(declaration.descriptor.as_slice())
        .map_err(|error| format!("the settings descriptor is not a DescribeResponse: {error}"))?;
    let settings = described
        .settings
        .into_iter()
        .map(setting_from_wire)
        .collect::<Result<Vec<_>, _>>()?;
    let schema: Value = serde_json::from_str(&declaration.json_schema)
        .map_err(|error| format!("the settings JSON Schema is not JSON: {error}"))?;
    let validator = SchemaValidator::new(schema)?;
    let base: Value = serde_json::from_str(&declaration.base_json)
        .map_err(|error| format!("the settings base is not JSON: {error}"))?;
    if !base.is_object() {
        return Err("the settings base must be a JSON object".to_owned());
    }
    Ok(DynamicSection {
        id: section_id(connector),
        path: section_path(connector),
        title: format!("{} ({platform})", connector.0),
        description: format!("The {} connector's own settings", connector.0),
        settings,
        validator: Arc::new(validator),
        base,
        base_source: format!("connector {}", connector.0),
    })
}

/// Whether `key` is one of [`LIVE_SAFE`] or below one.
fn is_live_safe(key: &str) -> bool {
    LIVE_SAFE.iter().any(|safe| {
        key == *safe
            || key
                .strip_prefix(safe)
                .is_some_and(|rest| rest.starts_with('.'))
    })
}

fn setting_from_wire(setting: proto::SettingDescriptor) -> Result<RuntimeSetting, String> {
    if !is_live_safe(&setting.key) {
        return Err(format!(
            "`{}` cannot be a runtime setting: only {} (and keys below them) can change while \
             a connector runs",
            setting.key,
            LIVE_SAFE.join(", "),
        ));
    }
    let kind = setting
        .kind
        .map(kind_from_wire)
        .transpose()?
        .unwrap_or(Kind::Json);
    let mut runtime = RuntimeSetting::owned(setting.key, setting.description, kind);
    if !setting.title.is_empty() {
        runtime.title = Some(Shared::from(setting.title));
    }
    runtime.per_entry = setting.per_entry;
    Ok(runtime)
}

fn choices_from_wire(choices: Vec<proto::Choice>) -> Choices {
    Choices::Fixed(Shared::from(
        choices
            .into_iter()
            .map(|choice| FixedChoice {
                value: Shared::from(choice.value),
                label: Shared::from(choice.label),
            })
            .collect::<Vec<_>>(),
    ))
}

/// A kind as the connector described it. Choices come resolved, so they
/// become fixed choices.
fn kind_from_wire(kind: proto::Kind) -> Result<Kind, String> {
    use proto::kind::Kind as Wire;

    let Some(kind) = kind.kind else {
        return Ok(Kind::Json);
    };
    Ok(match kind {
        Wire::Json(_) => Kind::Json,
        Wire::Text(text) => Kind::Text {
            optional: text.optional,
        },
        Wire::Number(number) => {
            if number.min > number.max {
                return Err(format!(
                    "a number setting's min ({}) is above its max ({})",
                    number.min, number.max
                ));
            }
            Kind::Number {
                min: number.min,
                max: number.max,
                unit: Shared::from(number.unit),
                suggestions: Shared::from(number.suggestions),
                optional: number.optional,
            }
        }
        Wire::OneOf(one_of) => Kind::OneOf {
            choices: choices_from_wire(one_of.choices),
            custom: one_of.custom,
            optional: one_of.optional,
        },
        Wire::SetOf(set_of) => Kind::SetOf {
            choices: choices_from_wire(set_of.choices),
            inverted: set_of.inverted,
        },
        Wire::Ref(reference) => Kind::Ref {
            domain: Shared::from(reference.domain),
            many: reference.many,
        },
        Wire::Form(form) => {
            let fields = form
                .fields
                .into_iter()
                .map(|field| {
                    Ok(Field {
                        key: Shared::from(field.key),
                        title: Shared::from(field.title),
                        kind: field
                            .kind
                            .map(kind_from_wire)
                            .transpose()?
                            .unwrap_or(Kind::Json),
                        group: (!field.group.is_empty()).then(|| Shared::from(field.group)),
                        required: field.required,
                        when: field
                            .when
                            .map(|when| (Shared::from(when.key), Shared::from(when.value))),
                    })
                })
                .collect::<Result<Vec<_>, String>>()?;
            Kind::Form(Shared::new(Form {
                fields: Shared::from(fields),
                initial: Shared::from(form.initial_json),
            }))
        }
        Wire::Map(map) => {
            let proto::MapKind {
                limited_names,
                names,
                value,
            } = *map;
            Kind::Map {
                names: limited_names.then(|| choices_from_wire(names)),
                value: Shared::new(
                    value
                        .map(|value| kind_from_wire(*value))
                        .transpose()?
                        .unwrap_or(Kind::Json),
                ),
            }
        }
    })
}

/// Validates a connector's section with the JSON Schema it declared.
///
/// Values chosen on a settings panel arrive as text (a choice's value is a
/// string), so before validating, a string where the schema wants a boolean
/// or a number is read as one (`"true"`, `"42"`); the section's effective
/// value then has the schema's types.
struct SchemaValidator {
    schema: Value,
    compiled: jsonschema::Validator,
}

impl SchemaValidator {
    fn new(schema: Value) -> Result<Self, String> {
        let compiled = jsonschema::draft202012::new(&schema)
            .map_err(|error| format!("the settings JSON Schema does not compile: {error}"))?;
        Ok(Self { schema, compiled })
    }
}

impl Validator for SchemaValidator {
    fn validate(&self, section: &Value) -> Result<Value, String> {
        let mut value = section.clone();
        coerce(&self.schema, &mut value);
        let errors: Vec<_> = self
            .compiled
            .iter_errors(&value)
            .map(|error| {
                let path = error.instance_path().to_string();
                if path.is_empty() {
                    error.to_string()
                } else {
                    format!("{path}: {error}")
                }
            })
            .collect();
        if errors.is_empty() {
            Ok(value)
        } else {
            Err(errors.join("; "))
        }
    }
}

/// Reads strings as the booleans and numbers `schema` asks for, through
/// object properties.
fn coerce(schema: &Value, value: &mut Value) {
    if let (Some(properties), Value::Object(object)) = (
        schema.get("properties").and_then(Value::as_object),
        &mut *value,
    ) {
        for (key, field) in object.iter_mut() {
            if let Some(field_schema) = properties.get(key) {
                coerce(field_schema, field);
            }
        }
        return;
    }
    let Value::String(text) = &*value else {
        return;
    };
    let wants = |name: &str| match schema.get("type") {
        Some(Value::String(kind)) => kind == name,
        Some(Value::Array(kinds)) => kinds.iter().any(|kind| kind == name),
        _ => false,
    };
    let text = text.trim();
    if wants("boolean") {
        match text {
            "true" => *value = Value::Bool(true),
            "false" => *value = Value::Bool(false),
            _ => {}
        }
    } else if wants("integer") {
        if let Ok(number) = text.parse::<i64>() {
            *value = Value::from(number);
        }
    } else if wants("number")
        && let Ok(number) = text.parse::<f64>()
        && let Some(number) = serde_json::Number::from_f64(number)
    {
        *value = Value::Number(number);
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or_default()
}

/// A declaration from a registration frame.
pub(crate) fn declaration_from_wire(
    connector: &ConnectorId,
    declaration: wuphf_proto::connector::ConnectorSettingsDeclaration,
) -> SettingsDeclaration {
    SettingsDeclaration {
        connector_id: connector.clone(),
        descriptor: declaration.descriptor,
        json_schema: declaration.json_schema,
        base_json: declaration.base_json,
    }
}

#[cfg(test)]
mod tests;
