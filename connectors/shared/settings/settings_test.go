package settings

import (
	"context"
	"encoding/json"
	"sync"
	"testing"

	botconf "github.com/propheci/botconf/go/settings"
	"github.com/propheci/botconf/go/settingspb"
	"google.golang.org/protobuf/proto"
)

type rendering struct {
	EditMode         string `toml:"edit_mode" botconf:"Edit mode" doc:"How edits are shown" jsonschema:"enum=in_place,enum=repost"`
	MaxMessageLength uint32 `toml:"max_message_length" botconf:"Longest message" jsonschema:"maximum=4096"`
	AsFiles          bool   `toml:"attachments_as_files" botconf:"Attachments as files"`
	// Not tagged: not a runtime setting.
	Internal string `toml:"internal"`
}

type runtime struct {
	LogLevel  string    `toml:"log_level" botconf:"Log level"`
	Rendering rendering `toml:"rendering"`
}

func base() runtime {
	return runtime{
		LogLevel:  "info",
		Rendering: rendering{EditMode: "in_place", MaxMessageLength: 4096},
	}
}

func declared(t *testing.T) (*settingspb.DescribeResponse, map[string]any, map[string]any) {
	t.Helper()
	declaration, err := Declare(base())
	if err != nil {
		t.Fatalf("Declare() error = %v", err)
	}
	var described settingspb.DescribeResponse
	if err := proto.Unmarshal(declaration.GetDescriptor_(), &described); err != nil {
		t.Fatal(err)
	}
	var schema, values map[string]any
	if err := json.Unmarshal([]byte(declaration.GetJsonSchema()), &schema); err != nil {
		t.Fatal(err)
	}
	if err := json.Unmarshal([]byte(declaration.GetBaseJson()), &values); err != nil {
		t.Fatal(err)
	}
	return &described, schema, values
}

func TestDeclareListsOnlyTaggedFields(t *testing.T) {
	described, _, _ := declared(t)
	var keys []string
	for _, setting := range described.GetSettings() {
		keys = append(keys, setting.GetKey())
	}
	want := []string{"log_level", "rendering.edit_mode", "rendering.max_message_length", "rendering.attachments_as_files"}
	if len(keys) != len(want) {
		t.Fatalf("keys = %v, want %v", keys, want)
	}
	for index := range want {
		if keys[index] != want[index] {
			t.Fatalf("keys = %v, want %v", keys, want)
		}
	}
	editMode := described.GetSettings()[1]
	if editMode.GetTitle() != "Edit mode" || editMode.GetDescription() != "How edits are shown" {
		t.Fatalf("edit mode = %v", editMode)
	}
	if choices := editMode.GetKind().GetOneOf().GetChoices(); len(choices) != 2 || choices[1].GetValue() != "repost" {
		t.Fatalf("edit mode choices = %v", choices)
	}
	if number := described.GetSettings()[2].GetKind().GetNumber(); number.GetMin() != 0 || number.GetMax() != 4096 {
		t.Fatalf("max message length kind = %v", number)
	}
	if choices := described.GetSettings()[3].GetKind().GetOneOf().GetChoices(); len(choices) != 2 || choices[0].GetValue() != "true" {
		t.Fatalf("attachments as files choices = %v", choices)
	}
}

func TestDeclareSchemaUsesTomlNames(t *testing.T) {
	_, schema, values := declared(t)
	properties, _ := schema["properties"].(map[string]any)
	renderingSchema, _ := properties["rendering"].(map[string]any)
	renderingProperties, _ := renderingSchema["properties"].(map[string]any)
	editMode, ok := renderingProperties["edit_mode"].(map[string]any)
	if !ok {
		t.Fatalf("schema has no rendering.edit_mode: %v", schema)
	}
	if enum, _ := editMode["enum"].([]any); len(enum) != 2 {
		t.Fatalf("edit_mode enum = %v", editMode["enum"])
	}
	if _, ok := properties["log_level"]; !ok {
		t.Fatalf("schema has no log_level: %v", schema)
	}
	renderingValues, _ := values["rendering"].(map[string]any)
	if values["log_level"] != "info" || renderingValues["edit_mode"] != "in_place" {
		t.Fatalf("base = %v", values)
	}
}

func TestLiveSwapIsAtomic(t *testing.T) {
	live := NewLive(base())
	var wait sync.WaitGroup
	for writer := range 4 {
		wait.Add(1)
		go func() {
			defer wait.Done()
			for range 200 {
				next := base()
				if writer%2 == 0 {
					next.LogLevel = "debug"
					next.Rendering.EditMode = "repost"
				}
				live.Set(next)
			}
		}()
	}
	for range 4 {
		wait.Add(1)
		go func() {
			defer wait.Done()
			for range 200 {
				// One Load is one consistent value: both fields come from the
				// same store.
				value := live.Load()
				if (value.LogLevel == "debug") != (value.Rendering.EditMode == "repost") {
					t.Errorf("torn value %+v", value)
					return
				}
			}
		}()
	}
	wait.Wait()
}

// fakeFollower replays the given events.
type fakeFollower struct {
	events   []botconf.Event
	prefixes []string
}

func (f *fakeFollower) Watch(ctx context.Context, _ *botconf.Actor, prefixes ...string) <-chan botconf.Event {
	f.prefixes = prefixes
	events := make(chan botconf.Event)
	go func() {
		defer close(events)
		for _, event := range f.events {
			select {
			case events <- event:
			case <-ctx.Done():
				return
			}
		}
	}()
	return events
}

func value(key, json string) botconf.Value {
	return botconf.Value{Key: "connector_settings.telegram-main." + key, JSON: []byte(json)}
}

func TestWatchAppliesPushedValues(t *testing.T) {
	follower := &fakeFollower{events: []botconf.Event{
		// The whole section after connecting.
		{Initial: true, Values: []botconf.Value{
			value("log_level", `"warn"`),
			value("rendering.edit_mode", `"in_place"`),
		}},
		// One change.
		{Values: []botconf.Value{value("rendering.edit_mode", `"repost"`)}},
		// A reconnect: everything starts again from the base.
		{Initial: true, Values: []botconf.Value{value("rendering.max_message_length", `100`)}},
	}}
	live := NewLive(base())
	var seen []runtime
	Watch(context.Background(), follower, "telegram-main", base(), live, func(_, next runtime) {
		seen = append(seen, next)
	}, func(err error) { t.Errorf("unexpected error: %v", err) })

	if len(follower.prefixes) != 1 || follower.prefixes[0] != "connector_settings.telegram-main" {
		t.Fatalf("watched %v", follower.prefixes)
	}
	if len(seen) != 3 {
		t.Fatalf("changes = %d, want 3", len(seen))
	}
	if seen[0].LogLevel != "warn" || seen[0].Rendering.EditMode != "in_place" {
		t.Fatalf("after the first event: %+v", seen[0])
	}
	if seen[1].LogLevel != "warn" || seen[1].Rendering.EditMode != "repost" {
		t.Fatalf("a change keeps the other values: %+v", seen[1])
	}
	final := live.Load()
	if final.LogLevel != "info" || final.Rendering.EditMode != "in_place" || final.Rendering.MaxMessageLength != 100 {
		t.Fatalf("after a reconnect: %+v", final)
	}
}

func TestWatchSkipsValuesThatDoNotDecode(t *testing.T) {
	follower := &fakeFollower{events: []botconf.Event{
		{Values: []botconf.Value{value("rendering.max_message_length", `"many"`)}},
	}}
	live := NewLive(base())
	var failures int
	Watch(context.Background(), follower, "telegram-main", base(), live, nil, func(error) { failures++ })
	if failures != 1 || live.Load() != base() {
		t.Fatalf("failures = %d, value = %+v", failures, live.Load())
	}
}
