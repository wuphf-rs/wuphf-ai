// Package settings lets a connector or a plugin declare its runtime settings
// to the core and follow the values the core pushes back (plans 040, 078).
//
// A connector keeps the settings that can change while it runs in one struct
// (its Runtime config), loaded from its TOML file as before. Declare describes
// that struct for the registration frame: a botconf descriptor of the fields
// tagged `botconf:"<title>"`, a JSON Schema of the whole struct, and the file
// values as the base. The core stores and validates every change; Watch follows
// the connector's section through the core's settings service and swaps each
// new value into a Live, which the connector reads once per use.
//
// The settings are the struct's options botconf's describe package marks
// runtime: field names come from the `toml` tags, so the core's keys are the
// file's (`rendering.edit_mode`); `botconf:"Edit mode"` declares a field with
// this title, `doc:"…"` describes it (its first paragraph is the panel's
// description), and `jsonschema:"enum=a,enum=b"` offers choices (`minimum=`
// and `maximum=` bound a number). The same tags describe the whole config
// file (see the setup package).
package settings

import (
	"context"
	"encoding/json"
	"fmt"
	"math"
	"sync/atomic"

	"github.com/invopop/jsonschema"
	"github.com/pelletier/go-toml/v2"
	"github.com/propheci/botconf/go/describe"
	botconf "github.com/propheci/botconf/go/settings"
	"github.com/propheci/botconf/go/settingspb"
	wuphfpb "github.com/wuphf-rs/wuphf-ai/proto/go/connector/v1"
	"google.golang.org/protobuf/proto"
)

// Namespace is where the core keeps connectors' settings:
// `connector_settings.<connector id>`.
const Namespace = "connector_settings"

// Prefix is the key of a connector's settings section.
func Prefix(connectorID string) string {
	return Namespace + "." + connectorID
}

// PluginNamespace is where the core keeps plugins' settings (wuphf-misc's):
// `plugin_settings.<plugin id>`.
const PluginNamespace = "plugin_settings"

// PluginPrefix is the key of a plugin's settings section.
func PluginPrefix(pluginID string) string {
	return PluginNamespace + "." + pluginID
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
	described, err := describe.Of("", "", base)
	if err != nil {
		return nil, err
	}
	descriptor, err := proto.Marshal(&settingspb.DescribeResponse{Settings: described.RuntimeSettings()})
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

// Watch follows the connector's section (see WatchSection).
func Watch[T any](
	ctx context.Context,
	follower Follower,
	connectorID string,
	base T,
	live *Live[T],
	onChange func(old, new T),
	onError func(error),
) {
	WatchSection(ctx, follower, Prefix(connectorID), base, live, onChange, onError)
}

// WatchSection follows the section at prefix (a connector's or a plugin's)
// and stores each pushed value in live, until ctx ends. After every
// (re)connect the core sends the whole section, so the value starts again
// from base and the pushed values are laid over it; later events carry only
// what changed. onChange, when set, runs after each swap with the old and the
// new value. A value that does not decode into T is reported through onError
// (when set) and skipped.
func WatchSection[T any](
	ctx context.Context,
	follower Follower,
	prefix string,
	base T,
	live *Live[T],
	onChange func(old, new T),
	onError func(error),
) {
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
