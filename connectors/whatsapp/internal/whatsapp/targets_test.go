package whatsappconnector

import (
	"testing"

	"github.com/akshettrj/wuphf/connectors/whatsapp/internal/config"
	"go.mau.fi/whatsmeow/types"
)

func testRegistry(t *testing.T) TargetRegistry {
	t.Helper()
	registry, err := NewTargetRegistry(config.File{
		CoreAddress:     "127.0.0.1:50051",
		ConnectorID:     "whatsapp-main",
		SessionDatabase: "file:session.db?_foreign_keys=on",
		Endpoints:       []string{"endpoint-a"},
		Targets: []config.Target{{
			EndpointID: "endpoint-a",
			GroupID:    "group-a",
			ChatJID:    "123456789@g.us",
		}},
	})
	if err != nil {
		t.Fatalf("NewTargetRegistry() error = %v", err)
	}
	return registry
}

func TestTargetRegistryResolvesChatJID(t *testing.T) {
	t.Parallel()
	target, ok := testRegistry(t).Incoming(types.NewJID("123456789", types.GroupServer))
	if !ok || target.EndpointID != "endpoint-a" {
		t.Fatalf("Incoming() = %#v, %v", target, ok)
	}
}
