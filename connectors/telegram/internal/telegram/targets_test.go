package telegramconnector

import (
	"testing"

	"github.com/akshettrj/wuphf/connectors/telegram/internal/config"
	"github.com/gotd/td/tg"
)

func TestTargetRegistryIndexesInboundAndOutboundTargets(t *testing.T) {
	t.Parallel()

	target := config.Target{
		EndpointID:       "endpoint-a",
		GroupID:          "group-a",
		Peer:             "example",
		IncomingPeerType: "channel",
		IncomingPeerID:   99,
	}
	registry := testRegistry(t, target)

	outbound, ok := registry.Outbound("endpoint-a")
	if !ok || outbound != target {
		t.Fatalf("Outbound() = %#v, %v", outbound, ok)
	}
	inbound := registry.Incoming(&tg.PeerChannel{ChannelID: 99})
	if len(inbound) != 1 || inbound[0] != target {
		t.Fatalf("Incoming() = %#v", inbound)
	}
}
