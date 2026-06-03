package instagramconnector

import (
	"context"
	"errors"
	"fmt"
	"strconv"
	"time"

	coreclient "github.com/akshettrj/wuphf/connectors/instagram/internal/core"
	wuphfpb "github.com/akshettrj/wuphf/proto/go/connector/v1"
	"go.mau.fi/mautrix-meta/pkg/messagix/methods"
	"go.mau.fi/mautrix-meta/pkg/messagix/socket"
	"go.mau.fi/mautrix-meta/pkg/messagix/table"
)

const sendReadyTimeout = 15 * time.Second

// ErrSendRejected is returned when Instagram accepts the task but reports the
// message as failed, which cannot be retried into success.
var ErrSendRejected = errors.New("instagram rejected message")

// SendClient is the subset of the messagix client the sender drives.
type SendClient interface {
	WaitUntilCanSendMessages(ctx context.Context, timeout time.Duration) error
	ExecuteTasks(ctx context.Context, tasks ...socket.Task) (*table.LSTable, error)
}

type Sender struct {
	client SendClient
}

func NewSender(client SendClient) Sender {
	return Sender{client: client}
}

// SendMessage delivers a text dispatch to the target Instagram thread. Instagram
// has no topics, so the dispatch never carries a thread target and the returned
// threadID is always empty.
func (s Sender) SendMessage(ctx context.Context, dispatch *wuphfpb.MessageCreateDispatch) (string, string, error) {
	threadID, err := strconv.ParseInt(dispatch.GetTargetChatId(), 10, 64)
	if err != nil {
		return "", "", coreclient.Permanent(fmt.Errorf("parse Instagram target thread %q: %w", dispatch.GetTargetChatId(), err))
	}
	if len(dispatch.Attachments) > 0 {
		return "", "", coreclient.Permanent(errors.New("Instagram attachment dispatch is not implemented"))
	}
	if dispatch.GetText() == "" {
		return "", "", coreclient.Permanent(errors.New("Instagram dispatch has no text to send"))
	}
	if err := s.client.WaitUntilCanSendMessages(ctx, sendReadyTimeout); err != nil {
		return "", "", fmt.Errorf("wait for Instagram send readiness: %w", err)
	}
	otid := methods.GenerateEpochID()
	task := &socket.SendMessageTask{
		ThreadId:         threadID,
		Otid:             otid,
		Source:           table.MESSENGER_INBOX_IN_THREAD,
		InitiatingSource: table.FACEBOOK_INBOX,
		SendType:         table.TEXT,
		SyncGroup:        1,
		Text:             dispatch.GetText(),
	}
	response, err := s.client.ExecuteTasks(ctx, task)
	if err != nil {
		return "", "", fmt.Errorf("send Instagram message: %w", err)
	}
	messageID, err := resolveSentMessageID(response, strconv.FormatInt(otid, 10))
	if err != nil {
		return "", "", err
	}
	return messageID, "", nil
}

// resolveSentMessageID extracts the canonical message id Instagram assigned to
// the optimistic send identified by otid. A failure reported for the same otid is
// permanent (the server rejected the content), so it is not retried.
func resolveSentMessageID(response *table.LSTable, otid string) (string, error) {
	if response == nil {
		return "", coreclient.Permanent(fmt.Errorf("%w: empty send response", ErrSendRejected))
	}
	for _, replaced := range response.LSReplaceOptimsiticMessage {
		if replaced.OfflineThreadingId == otid {
			return replaced.MessageId, nil
		}
	}
	for _, failed := range response.LSMarkOptimisticMessageFailed {
		if failed.OTID == otid {
			return "", coreclient.Permanent(fmt.Errorf("%w: %s", ErrSendRejected, failed.Message))
		}
	}
	for _, failed := range response.LSHandleFailedTask {
		if failed.OTID == otid {
			return "", coreclient.Permanent(fmt.Errorf("%w: %s", ErrSendRejected, failed.Message))
		}
	}
	return "", coreclient.Permanent(fmt.Errorf("%w: send response carried no message id", ErrSendRejected))
}

// EditMessage is not implemented for the Instagram MVP. Reporting it permanent
// lets the core dead-letter the dispatch instead of redelivering it forever.
func (s Sender) EditMessage(context.Context, *wuphfpb.MessageEditDispatch) (string, error) {
	return "", coreclient.Permanent(errors.New("Instagram edit is not implemented"))
}

// DeleteMessage is not implemented for the Instagram MVP. Reporting it permanent
// lets the core dead-letter the dispatch instead of redelivering it forever.
func (s Sender) DeleteMessage(context.Context, *wuphfpb.MessageDeleteDispatch) (string, error) {
	return "", coreclient.Permanent(errors.New("Instagram delete is not implemented"))
}
