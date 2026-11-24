//! Settings sections other processes declare: connectors when they register
//! (plan 040, [`crate::connector_settings`]) and plugins such as wuphf-misc
//! (plan 078, [`crate::plugin_settings`]).
//!
//! A declaration is a botconf descriptor of the settings, a JSON Schema of the
//! section and the declarer's own file values. The core turns it into a
//! botconf section, so the settings store keeps, validates and publishes the
//! declarer's settings like its own, and the declarer follows them through the
//! settings service's `Watch`. Each declaration is also stored, so the section
//! exists at startup, before its declarer returns.

use botconf::{
    Choices, DynamicSection, Field, FixedChoice, Form, Kind, RuntimeSetting, Shared, Validator,
};
use botconf_grpc::proto;
use prost::Message;
use serde_json::Value;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use wuphf_settings::SettingsStore;
use wuphf_storage::{SettingsDeclaration, SettingsDeclarations, SettingsOwner, StorageError};

/// Where a declared section goes and how the panel names it.
pub(crate) struct SectionSpec {
    pub(crate) id: String,
    pub(crate) path: String,
    pub(crate) title: String,
    pub(crate) description: String,
    pub(crate) base_source: String,
}

/// The botconf section a declaration describes: its settings, each key
/// accepted by `check_key`, validated by its JSON Schema, over its own values.
pub(crate) fn declared_section(
    spec: SectionSpec,
    declaration: &SettingsDeclaration,
    check_key: &dyn Fn(&str) -> Result<(), String>,
) -> Result<DynamicSection, String> {
    let described = proto::DescribeResponse::decode(declaration.descriptor.as_slice())
        .map_err(|error| format!("the settings descriptor is not a DescribeResponse: {error}"))?;
    let settings = described
        .settings
        .into_iter()
        .map(|setting| {
            check_key(&setting.key)?;
            setting_from_wire(setting)
        })
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
        id: spec.id,
        path: spec.path,
        title: spec.title,
        description: spec.description,
        settings,
        validator: Arc::new(validator),
        base,
        base_source: spec.base_source,
    })
}

/// Registers the section of every stored declaration `section` accepts (it
/// returns `None` for another owner kind), so stored overrides apply before
/// the declarers return. A declaration that no longer fits is skipped with a
/// warning; its overrides wait for its declarer.
pub(crate) async fn restore(
    store: &SettingsStore,
    declarations: &dyn SettingsDeclarations,
    section: impl Fn(&SettingsDeclaration) -> Option<Result<DynamicSection, String>>,
) -> Result<(), StorageError> {
    for declaration in declarations.settings_declarations().await? {
        let Some(built) = section(&declaration) else {
            continue;
        };
        let registered = match built {
            Ok(section) => store
                .register_section(section)
                .await
                .map(drop)
                .map_err(|error| error.to_string()),
            Err(error) => Err(error),
        };
        if let Err(error) = registered {
            tracing::warn!(
                owner = declaration.owner.kind(),
                id = declaration.owner.id(),
                %error,
                "skipping a stored settings declaration",
            );
        }
    }
    Ok(())
}

/// Stores `declaration` and registers (or replaces) `section`, built from it.
pub(crate) async fn declare(
    store: &SettingsStore,
    declarations: &dyn SettingsDeclarations,
    declaration: &SettingsDeclaration,
    section: DynamicSection,
) -> Result<(), String> {
    declarations
        .put_settings_declaration(declaration, now_ms())
        .await
        .map_err(|error| error.to_string())?;
    store
        .register_section(section)
        .await
        .map(drop)
        .map_err(|error| error.to_string())
}

/// A declaration from the wire, owned by `owner`.
pub(crate) fn declaration_from_wire(
    owner: SettingsOwner,
    declaration: wuphf_proto::connector::ConnectorSettingsDeclaration,
) -> SettingsDeclaration {
    SettingsDeclaration {
        owner,
        descriptor: declaration.descriptor,
        json_schema: declaration.json_schema,
        base_json: declaration.base_json,
    }
}

fn setting_from_wire(setting: proto::SettingDescriptor) -> Result<RuntimeSetting, String> {
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

/// A kind as the declarer described it. Choices come resolved, so they
/// become fixed choices.
pub(crate) fn kind_from_wire(kind: proto::Kind) -> Result<Kind, String> {
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

/// Validates a declared section with the JSON Schema it declared.
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
