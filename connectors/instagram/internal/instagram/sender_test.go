package instagramconnector

import (
	"context"
	"errors"
	"testing"

	coreclient "github.com/wuphf-rs/wuphf-ai/connectors/shared/coreclient"
	wuphfpb "github.com/wuphf-rs/wuphf-ai/proto/go/connector/v1"
	"go.mau.fi/mautrix-meta/pkg/instameow"
	"go.mau.fi/mautrix-meta/pkg/instameow/slidetypes"
)

type stubSendClient struct {
	sent      []*slidetypes.SendTextRequest
	edits     []*slidetypes.EditMessageRequest
	unsends   []*slidetypes.UnsendMessageRequest
	reactions []*slidetypes.CreateReactionRequest
	sentID    string
	err       error
}

func (s *stubSendClient) SendMessage(_ context.Context, req *slidetypes.SendTextRequest) (*slidetypes.SendTextResponse, error) {
	s.sent = append(s.sent, req)
	return &slidetypes.SendTextResponse{Message: slidetypes.SentMessage{ID: s.sentID}}, s.err
}

func (s *stubSendClient) EditMessage(_ context.Context, req *slidetypes.EditMessageRequest) (*slidetypes.EditMessageResponse, error) {
	s.edits = append(s.edits, req)
	return &slidetypes.EditMessageResponse{}, s.err
}

func (s *stubSendClient) UnsendMessage(_ context.Context, req *slidetypes.UnsendMessageRequest) (*slidetypes.UnsendMessageResponse, error) {
	s.unsends = append(s.unsends, req)
	return &slidetypes.UnsendMessageResponse{}, s.err
}

func (s *stubSendClient) SendReaction(_ context.Context, req *slidetypes.CreateReactionRequest) (*slidetypes.SendReactionResponse, error) {
	s.reactions = append(s.reactions, req)
	return &slidetypes.SendReactionResponse{}, s.err
}

func newTestSender(client *stubSendClient) (Sender, *stubThreads) {
	threads, fetcher := newTestDirectory()
	fetcher.ids[dmKey] = &instameow.ThreadIGIDs{ShortID: "short-77", LongID: "long-77"}
	return NewSender(client, threads), fetcher
}

func TestSendMessageAddressesShortIDWithReply(t *testing.T) {
	t.Parallel()
	client := &stubSendClient{sentID: "mid.sent"}
	sender, fetcher := newTestSender(client)
	text, reply := "hello", "mid.0"
	id, thread, err := sender.SendMessage(context.Background(), &wuphfpb.MessageCreateDispatch{
		TargetChatId: "42", Text: &text, ReplyToPlatformMessageId: &reply,
	})
	if err != nil || id != "mid.sent" || thread != "" {
		t.Fatalf("SendMessage() = %q, %q, %v", id, thread, err)
	}
	req := client.sent[0]
	if *req.IGThreadIGID != groupShortID || req.Text.Value != "hello" || *req.ReplyToMessageID != "mid.0" || req.OfflineThreadingID == "" {
		t.Fatalf("request = %+v", req)
	}
	if fetcher.fetchIDs != 0 {
		t.Fatalf("fetches = %d; a known thread must not cost a lookup", fetcher.fetchIDs)
	}
}

// A chat first addressed by a dispatch costs one id lookup, then is cached.
func TestSendMessageLooksUpUnknownThreadOnce(t *testing.T) {
	t.Parallel()
	client := &stubSendClient{sentID: "mid.sent"}
	sender, fetcher := newTestSender(client)
	text := "hi"
	for range 2 {
		if _, _, err := sender.SendMessage(context.Background(), &wuphfpb.MessageCreateDispatch{TargetChatId: "77", Text: &text}); err != nil {
			t.Fatalf("SendMessage() error = %v", err)
		}
	}
	if *client.sent[1].IGThreadIGID != "short-77" || fetcher.fetchIDs != 1 {
		t.Fatalf("thread = %q fetches = %d", *client.sent[1].IGThreadIGID, fetcher.fetchIDs)
	}
}

func TestSendMessagePermanentFailures(t *testing.T) {
	t.Parallel()
	text := "hi"
	cases := map[string]*wuphfpb.MessageCreateDispatch{
		"bad chat id":   {TargetChatId: "not-a-number", Text: &text},
		"no text":       {TargetChatId: "42"},
		"attachments":   {TargetChatId: "42", Text: &text, Attachments: []*wuphfpb.Attachment{{MediaId: "m"}}},
		"unknown chat":  {TargetChatId: "12345", Text: &text},
		"no message id": {TargetChatId: "42", Text: &text},
	}
	for name, dispatch := range cases {
		t.Run(name, func(t *testing.T) {
			t.Parallel()
			sender, _ := newTestSender(&stubSendClient{})
			_, _, err := sender.SendMessage(context.Background(), dispatch)
			var permanent coreclient.PermanentError
			if !errors.As(err, &permanent) {
				t.Fatalf("err = %v, want permanent", err)
			}
		})
	}
}

// A transport failure is retried, not dead-lettered.
func TestSendMessageTransportErrorIsTransient(t *testing.T) {
	t.Parallel()
	sender, _ := newTestSender(&stubSendClient{err: errors.New("timeout")})
	text := "hi"
	_, _, err := sender.SendMessage(context.Background(), &wuphfpb.MessageCreateDispatch{TargetChatId: "42", Text: &text})
	var permanent coreclient.PermanentError
	if err == nil || errors.As(err, &permanent) {
		t.Fatalf("err = %v, want transient", err)
	}
}

func TestEditMessageAddressesShortID(t *testing.T) {
	t.Parallel()
	client := &stubSendClient{}
	sender, _ := newTestSender(client)
	text := "fixed"
	id, err := sender.EditMessage(context.Background(), &wuphfpb.MessageEditDispatch{
		TargetChatId: "42", TargetPlatformMessageId: "mid.1", Text: &text,
	})
	if err != nil || id != "mid.1" {
		t.Fatalf("EditMessage() = %q, %v", id, err)
	}
	req := client.edits[0]
	if req.ThreadID != groupShortID || req.TargetMessageID != "mid.1" || req.Body.Value != "fixed" {
		t.Fatalf("request = %+v", req)
	}
}

func TestDeleteMessageAddressesLongID(t *testing.T) {
	t.Parallel()
	client := &stubSendClient{}
	sender, _ := newTestSender(client)
	id, err := sender.DeleteMessage(context.Background(), &wuphfpb.MessageDeleteDispatch{
		TargetChatId: "42", TargetPlatformMessageId: "mid.1",
	})
	if err != nil || id != "mid.1" {
		t.Fatalf("DeleteMessage() = %q, %v", id, err)
	}
	if req := client.unsends[0]; req.SendData.ThreadID != "long-42" || req.MessageID != "mid.1" {
		t.Fatalf("request = %+v", req)
	}
}

func TestApplyReactionSetsAndClears(t *testing.T) {
	t.Parallel()
	client := &stubSendClient{}
	sender, _ := newTestSender(client)
	for _, emoji := range []string{"👍", ""} {
		if _, err := sender.ApplyReaction(context.Background(), &wuphfpb.ReactionDispatch{
			TargetChatId: "42", TargetPlatformMessageId: "mid.1", Emoji: emoji,
		}); err != nil {
			t.Fatalf("ApplyReaction(%q) error = %v", emoji, err)
		}
	}
	set, clear := client.reactions[0].Input, client.reactions[1].Input
	if set.ReactionStatus != slidetypes.ReactionStatusCreated || set.Emoji != "👍" || set.ThreadID != groupShortID || set.MessageID != "mid.1" {
		t.Fatalf("set = %+v", set)
	}
	if clear.ReactionStatus != slidetypes.ReactionStatusDeleted {
		t.Fatalf("clear = %+v", clear)
	}
}
