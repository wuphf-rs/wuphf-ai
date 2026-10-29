// Package settings lets a connector declare its runtime settings to the core
// and follow the values the core pushes back (plan 040).
//
// A connector keeps the settings that can change while it runs in one struct
// (its Runtime config), loaded from its TOML file as before. Declare describes
// that struct for the registration frame: a botconf descriptor of the fields
// tagged `botconf:"<title>"`, a JSON Schema of the whole struct, and the file
// values as the base. The core stores and validates every change; Watch follows
// the connector's section through the core's settings service and swaps each
// new value into a Live, which the connector reads once per use.
//
// Field names come from the `toml` tags, so the core's keys are the file's
// (`rendering.edit_mode`). Tags read by Declare:
//
//   - `botconf:"Edit mode"`: declare the field, with this title; untagged
//     fields are not runtime settings (a struct field is declared through its
//     tagged fields).
//   - `doc:"…"`: the setting's description.
//   - `jsonschema:"enum=a,enum=b"`: the allowed values, offered as choices;
//     `minimum=` and `maximum=` bound a number.
package settings

import (
	"context"
	"encoding/json"
	"fmt"
	"math"
	"reflect"
	"strconv"
	"strings"
	"sync/atomic"

	wuphfpb "github.com/akshettrj/wuphf/proto/go/connector/v1"
	"github.com/invopop/jsonschema"
	"github.com/pelletier/go-toml/v2"
	botconf "github.com/propheci/botconf/go/settings"
	"github.com/propheci/botconf/go/settingspb"
	"google.golang.org/protobuf/proto"
)

// Namespace is where the core keeps connectors' settings:
// `connector_settings.<connector id>`.
const Namespace = "connector_settings"

// Prefix is the key of a connector's settings section.
func Prefix(connectorID string) string {
	return Namespace + "." + connectorID
}

// Live holds a connector's current runtime settings. Readers call Load once
// per use, so one use sees one consistent value; Watch swaps in each new value
// pushed by the core.
type Live[T any] struct {
	value atomic.Pointer[T]
}

// NewLive holds initial until the core pushes another value.
func NewLive[T any](initial T) *Live[T] {
	live := &Live[T]{}
	live.value.Store(&initial)
	return live
}

// Load returns the current value.
func (l *Live[T]) Load() T {
	return *l.value.Load()
}

// Set replaces the value and returns the previous one. Watch calls it for
// each push; a connector's tests can call it to change a setting.
func (l *Live[T]) Set(value T) (old T) {
	return *l.value.Swap(&value)
}

// Declare describes base's type for the registration frame: the settings to
// declare (the `botconf`-tagged fields), a JSON Schema of the whole type, and
// base as the section's values.
func Declare[T any](base T) (*wuphfpb.ConnectorSettingsDeclaration, error) {
	settings, err := describe(reflect.TypeOf(base), "")
	if err != nil {
		return nil, err
	}
	descriptor, err := proto.Marshal(&settingspb.DescribeResponse{Settings: settings})
	if err != nil {
		return nil, fmt.Errorf("encode the settings descriptor: %w", err)
	}
	reflector := jsonschema.Reflector{
		FieldNameTag:   "toml",
		DoNotReference: true,
		ExpandedStruct: true,
		// The base holds every field, so every field is present; nothing needs
		// to be listed as required.
		RequiredFromJSONSchemaTags: true,
	}
	schema, err := json.Marshal(reflector.Reflect(&base))
	if err != nil {
		return nil, fmt.Errorf("encode the settings schema: %w", err)
	}
	values, err := toMap(base)
	if err != nil {
		return nil, err
	}
	baseJSON, err := json.Marshal(values)
	if err != nil {
		return nil, fmt.Errorf("encode the settings base: %w", err)
	}
	return &wuphfpb.ConnectorSettingsDeclaration{
		Descriptor_: descriptor,
		JsonSchema:  string(schema),
		BaseJson:    string(baseJSON),
	}, nil
}

// describe lists the botconf-tagged fields of a struct type, their keys below
// prefix.
func describe(kind reflect.Type, prefix string) ([]*settingspb.SettingDescriptor, error) {
	if kind.Kind() != reflect.Struct {
		return nil, fmt.Errorf("settings must be a struct, not %s", kind)
	}
	var settings []*settingspb.SettingDescriptor
	for index := range kind.NumField() {
		field := kind.Field(index)
		name := tomlName(field)
		if name == "" || !field.IsExported() {
			continue
		}
		key := name
		if prefix != "" {
			key = prefix + "." + name
		}
		if field.Type.Kind() == reflect.Struct {
			nested, err := describe(field.Type, key)
			if err != nil {
				return nil, err
			}
			settings = append(settings, nested...)
			continue
		}
		title, ok := field.Tag.Lookup("botconf")
		if !ok {
			continue
		}
		settingKind, err := kindOf(field)
		if err != nil {
			return nil, fmt.Errorf("setting %s: %w", key, err)
		}
		settings = append(settings, &settingspb.SettingDescriptor{
			Key:         key,
			Title:       title,
			Description: field.Tag.Get("doc"),
			Kind:        settingKind,
		})
	}
	return settings, nil
}

// tomlName is the field's name in the file, or "" for a skipped field.
func tomlName(field reflect.StructField) string {
	name, _, _ := strings.Cut(field.Tag.Get("toml"), ",")
	if name == "-" {
		return ""
	}
	if name == "" {
		return field.Name
	}
	return name
}

// schemaTags reads the `jsonschema` tag's enum, minimum and maximum.
func schemaTags(field reflect.StructField) (enum []string, minimum, maximum *int64) {
	for _, part := range strings.Split(field.Tag.Get("jsonschema"), ",") {
		name, value, _ := strings.Cut(part, "=")
		switch name {
		case "enum":
			enum = append(enum, value)
		case "minimum", "maximum":
			number, err := strconv.ParseInt(value, 10, 64)
			if err != nil {
				continue
			}
			if name == "minimum" {
				minimum = &number
			} else {
				maximum = &number
			}
		}
	}
	return enum, minimum, maximum
}

func choices(values ...string) []*settingspb.Choice {
	out := make([]*settingspb.Choice, 0, len(values))
	for _, value := range values {
		out = append(out, &settingspb.Choice{Value: value, Label: value})
	}
	return out
}

// kindOf picks the editor for a field from its Go type and tags.
func kindOf(field reflect.StructField) (*settingspb.Kind, error) {
	enum, minimum, maximum := schemaTags(field)
	switch field.Type.Kind() {
	case reflect.String:
		if len(enum) > 0 {
			return &settingspb.Kind{Kind: &settingspb.Kind_OneOf{OneOf: &settingspb.OneOfKind{
				Choices: choices(enum...),
			}}}, nil
		}
		return &settingspb.Kind{Kind: &settingspb.Kind_Text{Text: &settingspb.TextKind{}}}, nil
	case reflect.Bool:
		return &settingspb.Kind{Kind: &settingspb.Kind_OneOf{OneOf: &settingspb.OneOfKind{
			Choices: []*settingspb.Choice{
				{Value: "true", Label: "✅ Yes"},
				{Value: "false", Label: "❌ No"},
			},
		}}}, nil
	case reflect.Int, reflect.Int8, reflect.Int16, reflect.Int32, reflect.Int64,
		reflect.Uint, reflect.Uint8, reflect.Uint16, reflect.Uint32, reflect.Uint64:
		low, high := bounds(field.Type)
		if minimum != nil {
			low = *minimum
		}
		if maximum != nil {
			high = *maximum
		}
		return &settingspb.Kind{Kind: &settingspb.Kind_Number{Number: &settingspb.NumberKind{
			Min: low,
			Max: high,
		}}}, nil
	default:
		return nil, fmt.Errorf("no settings editor for %s", field.Type)
	}
}

// bounds is the range an integer type holds, capped to int64.
func bounds(kind reflect.Type) (int64, int64) {
	switch kind.Kind() {
	case reflect.Uint, reflect.Uint8, reflect.Uint16, reflect.Uint32, reflect.Uint64:
		bits := kind.Bits()
		if bits >= 64 {
			return 0, math.MaxInt64
		}
		return 0, int64(1)<<bits - 1
	default:
		bits := kind.Bits()
		return -(int64(1) << (bits - 1)), int64(1)<<(bits-1) - 1
	}
}

// toMap is value as a map keyed by its TOML names.
func toMap[T any](value T) (map[string]any, error) {
	encoded, err := toml.Marshal(value)
	if err != nil {
		return nil, fmt.Errorf("encode settings: %w", err)
	}
	values := map[string]any{}
	if err := toml.Unmarshal(encoded, &values); err != nil {
		return nil, fmt.Errorf("decode settings: %w", err)
	}
	return values, nil
}

// fromMap reads values (keyed by TOML names) into a T.
func fromMap[T any](values map[string]any) (T, error) {
	var value T
	encoded, err := toml.Marshal(values)
	if err != nil {
		return value, fmt.Errorf("encode settings: %w", err)
	}
	if err := toml.Unmarshal(encoded, &value); err != nil {
		return value, fmt.Errorf("decode settings: %w", err)
	}
	return value, nil
}

// merge lays incoming over base, object by object.
func merge(base, incoming map[string]any) {
	for key, value := range incoming {
		if nested, ok := value.(map[string]any); ok {
			if existing, ok := base[key].(map[string]any); ok {
				merge(existing, nested)
				continue
			}
		}
		base[key] = value
	}
}

// Follower streams the values under some key prefixes; *botconf.Client is one.
type Follower interface {
	Watch(ctx context.Context, actor *botconf.Actor, prefixes ...string) <-chan botconf.Event
}

// Watch follows the connector's section and stores each pushed value in live,
// until ctx ends. After every (re)connect the core sends the whole section, so
// the value starts again from base and the pushed values are laid over it;
// later events carry only what changed. onChange, when set, runs after each
// swap with the old and the new value. A value that does not decode into T is
// reported through onError (when set) and skipped.
func Watch[T any](
	ctx context.Context,
	follower Follower,
	connectorID string,
	base T,
	live *Live[T],
	onChange func(old, new T),
	onError func(error),
) {
	prefix := Prefix(connectorID)
	for event := range follower.Watch(ctx, nil, prefix) {
		current := live.Load()
		if event.Initial {
			current = base
		}
		next, err := apply(current, event.Values, prefix)
		if err != nil {
			if onError != nil {
				onError(err)
			}
			continue
		}
		old := live.Set(next)
		if onChange != nil {
			onChange(old, next)
		}
	}
}

// apply lays the pushed values over current.
func apply[T any](current T, values []botconf.Value, prefix string) (T, error) {
	values = present(values)
	if len(values) == 0 {
		return current, nil
	}
	pushed := map[string]any{}
	if err := botconf.Decode(values, prefix, &pushed); err != nil {
		return current, err
	}
	merged, err := toMap(current)
	if err != nil {
		return current, err
	}
	merge(merged, integers(pushed).(map[string]any))
	return fromMap[T](merged)
}

// integers turns JSON's whole floats back into integers, so TOML decodes them
// into integer fields.
func integers(value any) any {
	switch value := value.(type) {
	case map[string]any:
		for key, nested := range value {
			value[key] = integers(nested)
		}
		return value
	case []any:
		for index, nested := range value {
			value[index] = integers(nested)
		}
		return value
	case float64:
		if value == math.Trunc(value) && math.Abs(value) < 1<<53 {
			return int64(value)
		}
		return value
	default:
		return value
	}
}

// present drops the values that have none (keys whose section went away).
func present(values []botconf.Value) []botconf.Value {
	kept := values[:0:0]
	for _, value := range values {
		if value.JSON != nil {
			kept = append(kept, value)
		}
	}
	return kept
}
