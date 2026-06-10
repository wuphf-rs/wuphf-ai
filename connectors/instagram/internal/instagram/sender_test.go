package instagramconnector

import (
	"context"
	"errors"
	"testing"
	"time"

	coreclient "github.com/akshettrj/wuphf/connectors/instagram/internal/core"
	wuphfpb "github.com/akshettrj/wuphf/proto/go/connector/v1"
	"go.mau.fi/mautrix-meta/pkg/messagix/socket"
	"go.mau.fi/mautrix-meta/pkg/messagix/table"
)

type stubSendClient struct{ tasks []socket.Task }

func (*stubSendClient) WaitUntilCanSendMessages(context.Context, time.Duration) error { return nil }

func (s *stubSendClient) ExecuteTasks(_ context.Context, tasks ...socket.Task) (*table.LSTable, error) {
	s.tasks = append(s.tasks, tasks...)
	return &table.LSTable{}, nil
}

func TestResolveSentMessageIDMatchesOtid(t *testing.T) {
	t.Parallel()
	response := &table.LSTable{
		LSReplaceOptimsiticMessage: []*table.LSReplaceOptimsiticMessage{
			{OfflineThreadingId: "other", MessageId: "wrong"},
			{OfflineThreadingId: "otid-1", MessageId: "mid-1"},
		},
	}
	id, err := resolveSentMessageID(response, "otid-1")
	if err != nil || id != "mid-1" {
		t.Fatalf("id = %q, err = %v", id, err)
	}
}

func TestResolveSentMessageIDReportsRejectionPermanent(t *testing.T) {
	t.Parallel()
	response := &table.LSTable{
		LSMarkOptimisticMessageFailed: []*table.LSMarkOptimisticMessageFailed{
			{OTID: "otid-1", Message: "blocked"},
		},
	}
	_, err := resolveSentMessageID(response, "otid-1")
	if err == nil || !errors.Is(err, ErrSendRejected) {
		t.Fatalf("err = %v, want ErrSendRejected", err)
	}
	var permanent coreclient.PermanentError
	if !errors.As(err, &permanent) {
		t.Fatalf("err = %v, want permanent", err)
	}
}

func TestResolveSentMessageIDMissingIsPermanent(t *testing.T) {
	t.Parallel()
	_, err := resolveSentMessageID(&table.LSTable{}, "otid-1")
	var permanent coreclient.PermanentError
	if err == nil || !errors.As(err, &permanent) {
		t.Fatalf("err = %v, want permanent", err)
	}
}

func TestEditMessageIssuesEditTask(t *testing.T) {
	t.Parallel()
	client := &stubSendClient{}
	sender := NewSender(client)
	text := "revised"
	id, err := sender.EditMessage(context.Background(), &wuphfpb.MessageEditDispatch{
		TargetPlatformMessageId: "m1",
		Text:                    &text,
	})
	if err != nil || id != "m1" {
		t.Fatalf("EditMessage() = (%q, %v)", id, err)
	}
	task, ok := client.tasks[0].(*socket.EditMessageTask)
	if !ok || task.MessageID != "m1" || task.Text != "revised" {
		t.Fatalf("task = %#v", client.tasks[0])
	}
}

func TestDeleteMessageIssuesUnsendTask(t *testing.T) {
	t.Parallel()
	client := &stubSendClient{}
	sender := NewSender(client)
	id, err := sender.DeleteMessage(context.Background(), &wuphfpb.MessageDeleteDispatch{
		TargetPlatformMessageId: "m1",
	})
	if err != nil || id != "m1" {
		t.Fatalf("DeleteMessage() = (%q, %v)", id, err)
	}
	task, ok := client.tasks[0].(*socket.DeleteMessageTask)
	if !ok || task.MessageId != "m1" {
		t.Fatalf("task = %#v", client.tasks[0])
	}
}

func TestEditMessageRejectsMissingTarget(t *testing.T) {
	t.Parallel()
	sender := NewSender(&stubSendClient{})
	text := "x"
	_, err := sender.EditMessage(context.Background(), &wuphfpb.MessageEditDispatch{Text: &text})
	var permanent coreclient.PermanentError
	if err == nil || !errors.As(err, &permanent) {
		t.Fatalf("err = %v, want permanent", err)
	}
}
