package config

import "testing"

func TestValidateRejectsAmbiguousChats(t *testing.T) {
	t.Parallel()
	cfg := File{
		CoreAddress:     "127.0.0.1:50051",
		ConnectorID:     "whatsapp-main",
		SessionDatabase: "file:session.db?_foreign_keys=on",
		Endpoints:       []string{"a", "b"},
		Targets: []Target{
			{EndpointID: "a", GroupID: "group", ChatJID: "123@g.us"},
			{EndpointID: "b", GroupID: "group", ChatJID: "123@g.us"},
		},
	}
	if err := cfg.Validate(); err == nil {
		t.Fatal("Validate() accepted ambiguous chat mapping")
	}
}
