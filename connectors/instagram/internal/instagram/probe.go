package instagramconnector

import (
	"context"
	"errors"
	"strconv"

	"github.com/wuphf-rs/wuphf-ai/connectors/shared/coreclient"
	wuphfpb "github.com/wuphf-rs/wuphf-ai/proto/go/connector/v1"
)

// prober answers the core's health probes (plan 079): a thread this account
// can address is one it is in and may write to. Instagram has no topics.
type prober struct {
	threads *ThreadDirectory
}

func (p prober) Probe(ctx context.Context, chatIDs []string) []*wuphfpb.ChatFacts {
	facts := make([]*wuphfpb.ChatFacts, 0, len(chatIDs))
	for _, chatID := range chatIDs {
		facts = append(facts, p.probe(ctx, chatID))
	}
	return facts
}

func (p prober) probe(ctx context.Context, chatID string) *wuphfpb.ChatFacts {
	facts := &wuphfpb.ChatFacts{
		ChatId:          chatID,
		Forum:           coreclient.Fact(false),
		CanCreateTopics: coreclient.Fact(false),
	}
	threadKey, err := strconv.ParseInt(chatID, 10, 64)
	if err != nil {
		facts.Detail = "an Instagram chat id is a thread key (a decimal number)"
		return facts
	}
	switch _, err := p.threads.addressable(ctx, threadKey); {
	case err == nil:
		facts.Exists, facts.Member, facts.CanSend = coreclient.Fact(true), coreclient.Fact(true), coreclient.Fact(true)
	case errors.Is(err, ErrThreadUnknown):
		facts.Exists, facts.Detail = coreclient.Fact(false), err.Error()
	default:
		facts.Detail = err.Error()
	}
	return facts
}
