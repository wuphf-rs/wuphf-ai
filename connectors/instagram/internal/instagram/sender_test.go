package instagramconnector

import (
	"errors"
	"testing"

	coreclient "github.com/akshettrj/wuphf/connectors/instagram/internal/core"
	"go.mau.fi/mautrix-meta/pkg/messagix/table"
)

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
