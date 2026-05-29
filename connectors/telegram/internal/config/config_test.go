package config

import "testing"

func TestValidateTargetMustBelongToConnector(t *testing.T) {
	t.Parallel()

	cfg := File{
		CoreAddress: "127.0.0.1:50051",
		ConnectorID: "telegram-main",
		SessionFile: "./telegram.session.json",
		Endpoints:   []string{"endpoint-a"},
		Targets: []Target{{
			EndpointID:       "endpoint-b",
			GroupID:          "group-a",
			Peer:             "example",
			IncomingPeerType: "channel",
			IncomingPeerID:   1001,
		}},
	}
	if err := cfg.Validate(); err == nil {
		t.Fatal("Validate() succeeded for unowned target")
	}
}

func TestValidateRejectsAmbiguousInboundTarget(t *testing.T) {
	t.Parallel()

	cfg := File{
		CoreAddress: "127.0.0.1:50051",
		ConnectorID: "telegram-main",
		SessionFile: "./telegram.session.json",
		Endpoints:   []string{"general", "topic"},
		Targets: []Target{
			{
				EndpointID:       "general",
				GroupID:          "group-a",
				Peer:             "example",
				IncomingPeerType: "channel",
				IncomingPeerID:   1001,
			},
			{
				EndpointID:         "topic",
				GroupID:            "group-b",
				Peer:               "example",
				IncomingPeerType:   "channel",
				IncomingPeerID:     1001,
				TopicRootMessageID: 12,
			},
		},
	}
	if err := cfg.Validate(); err == nil {
		t.Fatal("Validate() succeeded for overlapping general and topic targets")
	}
}
