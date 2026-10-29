package config

import (
	"encoding/base64"
	"encoding/json"
	"os"
	"testing"

	"github.com/akshettrj/wuphf/connectors/shared/settings"
)

// declarationFixture is what this connector declares by default, checked by
// the core's tests (crates/wuphf-runtime, connector_settings) so the Go schema
// and descriptor stay readable by the Rust side.
const declarationFixture = "../../testdata/settings_declaration.json"

// TestDeclarationFixtureIsCurrent regenerates the fixture with
// WUPHF_UPDATE_FIXTURES=1 and otherwise checks it is up to date.
func TestDeclarationFixtureIsCurrent(t *testing.T) {
	declaration, err := settings.Declare(Runtime{}.WithDefaults())
	if err != nil {
		t.Fatal(err)
	}
	encoded, err := json.MarshalIndent(map[string]string{
		"descriptor_base64": base64.StdEncoding.EncodeToString(declaration.GetDescriptor_()),
		"json_schema":       declaration.GetJsonSchema(),
		"base_json":         declaration.GetBaseJson(),
	}, "", "  ")
	if err != nil {
		t.Fatal(err)
	}
	encoded = append(encoded, '\n')
	if os.Getenv("WUPHF_UPDATE_FIXTURES") == "1" {
		if err := os.WriteFile(declarationFixture, encoded, 0o644); err != nil {
			t.Fatal(err)
		}
		return
	}
	current, err := os.ReadFile(declarationFixture)
	if err != nil {
		t.Fatalf("read %s (regenerate with WUPHF_UPDATE_FIXTURES=1): %v", declarationFixture, err)
	}
	if string(current) != string(encoded) {
		t.Fatalf("%s is stale; regenerate it with WUPHF_UPDATE_FIXTURES=1 go test ./internal/config/", declarationFixture)
	}
}
