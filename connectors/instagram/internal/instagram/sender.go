package instagramconnector

import (
	"context"
	"errors"
	"fmt"
	"strconv"

	coreclient "github.com/wuphf-rs/wuphf-ai/connectors/shared/coreclient"
	wuphfpb "github.com/wuphf-rs/wuphf-ai/proto/go/connector/v1"
	"go.mau.fi/mautrix-meta/pkg/instameow/slidetypes"
	"go.mau.fi/mautrix-meta/pkg/messagix/methods"
)

// sendAttribution is the surface Instagram web attributes in-thread sends to.
const sendAttribution = "igd_web_chat_tab:in_thread"

// ErrSendRejected is returned when Instagram accepts a send but reports no
// resulting message, which cannot be retried into success.
var ErrSendRejected = errors.New("instagram rejected message")

// SendClient is the subset of the instameow client the sender drives.
type SendClient interface {
	SendMessage(context.Context, *slidetypes.SendTextRequest) (*slidetypes.SendTextResponse, error)
	EditMessage(context.Context, *slidetypes.EditMessageRequest) (*slidetypes.EditMessageResponse, error)
	UnsendMessage(context.Context, *slidetypes.UnsendMessageRequest) (*slidetypes.UnsendMessageResponse, error)
	SendReaction(context.Context, *slidetypes.CreateReactionRequest) (*slidetypes.SendReactionResponse, error)
}

// ThreadAddresser resolves a chat's thread key to the ids Instagram's send
// APIs address. *ThreadDirectory satisfies it.
type ThreadAddresser interface {
	ShortID(ctx context.Context, threadKey int64) (string, error)
	LongID(ctx context.Context, threadKey int64) (string, error)
}

type Sender struct {
	client  SendClient
	threads ThreadAddresser
}

var _ coreclient.Reactor = Sender{}

func NewSender(client SendClient, threads ThreadAddresser) Sender {
	return Sender{client: client, threads: threads}
}

// SendMessage delivers a text dispatch to the target Instagram thread. Instagram
// has no topics, so the dispatch never carries a thread target and the returned
// threadID is always empty.
func (s Sender) SendMessage(ctx context.Context, dispatch *wuphfpb.MessageCreateDispatch) (string, string, error) {
	if len(dispatch.Attachments) > 0 {
		return "", "", coreclient.Permanent(errors.New("Instagram attachment dispatch is not implemented"))
	}
	if dispatch.GetText() == "" {
		return "", "", coreclient.Permanent(errors.New("Instagram dispatch has no text to send"))
	}
	threadID, err := s.shortID(ctx, dispatch.GetTargetChatId())
	if err != nil {
		return "", "", err
	}
	request := &slidetypes.SendTextRequest{
		IGThreadIGID:       &threadID,
		OfflineThreadingID: newOfflineThreadingID(),
		Text:               slidetypes.SensitiveString{Value: dispatch.GetText()},
		SendAttribution:    new(sendAttribution),
	}
	if reply := dispatch.GetReplyToPlatformMessageId(); reply != "" {
		request.ReplyToMessageID = &reply
	}
	response, err := s.client.SendMessage(ctx, request)
	if err != nil {
		return "", "", fmt.Errorf("send Instagram message: %w", err)
	}
	if response == nil || response.Message.ID == "" {
		return "", "", coreclient.Permanent(fmt.Errorf("%w: send response carried no message id", ErrSendRejected))
	}
	return response.Message.ID, "", nil
}

// EditMessage revises a message in place. Instagram keeps the message id across
// an edit, so the target id is returned unchanged.
func (s Sender) EditMessage(ctx context.Context, dispatch *wuphfpb.MessageEditDispatch) (string, error) {
	messageID := dispatch.GetTargetPlatformMessageId()
	if messageID == "" {
		return "", coreclient.Permanent(errors.New("Instagram edit dispatch has no target message id"))
	}
	if dispatch.GetText() == "" {
		return "", coreclient.Permanent(errors.New("Instagram edit dispatch has no text"))
	}
	threadID, err := s.shortID(ctx, dispatch.GetTargetChatId())
	if err != nil {
		return "", err
	}
	_, err = s.client.EditMessage(ctx, &slidetypes.EditMessageRequest{
		ThreadID:           threadID,
		TargetMessageID:    messageID,
		Body:               slidetypes.SensitiveString{Value: dispatch.GetText()},
		OfflineThreadingID: newOfflineThreadingID(),
	})
	if err != nil {
		return "", fmt.Errorf("edit Instagram message: %w", err)
	}
	return messageID, nil
}

// DeleteMessage unsends a message for everyone, the only deletion Instagram
// exposes that propagates to other participants. The deleted message id is
// returned unchanged.
func (s Sender) DeleteMessage(ctx context.Context, dispatch *wuphfpb.MessageDeleteDispatch) (string, error) {
	messageID := dispatch.GetTargetPlatformMessageId()
	if messageID == "" {
		return "", coreclient.Permanent(errors.New("Instagram delete dispatch has no target message id"))
	}
	threadKey, err := parseThreadKey(dispatch.GetTargetChatId())
	if err != nil {
		return "", err
	}
	threadID, err := s.threads.LongID(ctx, threadKey)
	if err != nil {
		return "", classifyThreadError(err)
	}
	_, err = s.client.UnsendMessage(ctx, &slidetypes.UnsendMessageRequest{
		MessageID: messageID,
		SendData:  slidetypes.SendData{ThreadID: threadID},
	})
	if err != nil {
		return "", fmt.Errorf("delete Instagram message: %w", err)
	}
	return messageID, nil
}

// ApplyReaction sets or clears the account's reaction on a delivered copy. An
// empty emoji clears it. Instagram allows one reaction per person per message,
// matching the core's per-reactor toggle.
func (s Sender) ApplyReaction(ctx context.Context, dispatch *wuphfpb.ReactionDispatch) (string, error) {
	messageID := dispatch.GetTargetPlatformMessageId()
	if messageID == "" {
		return "", coreclient.Permanent(errors.New("Instagram reaction dispatch has no target message id"))
	}
	threadID, err := s.shortID(ctx, dispatch.GetTargetChatId())
	if err != nil {
		return "", err
	}
	status := slidetypes.ReactionStatusCreated
	if dispatch.GetEmoji() == "" {
		status = slidetypes.ReactionStatusDeleted
	}
	_, err = s.client.SendReaction(ctx, &slidetypes.CreateReactionRequest{Input: slidetypes.ReactionInput{
		Emoji:          dispatch.GetEmoji(),
		MessageID:      messageID,
		ReactionStatus: status,
		ThreadID:       threadID,
	}})
	if err != nil {
		return "", fmt.Errorf("react to Instagram message: %w", err)
	}
	return messageID, nil
}

func (s Sender) shortID(ctx context.Context, chatID string) (string, error) {
	threadKey, err := parseThreadKey(chatID)
	if err != nil {
		return "", err
	}
	threadID, err := s.threads.ShortID(ctx, threadKey)
	if err != nil {
		return "", classifyThreadError(err)
	}
	return threadID, nil
}

func parseThreadKey(chatID string) (int64, error) {
	threadKey, err := strconv.ParseInt(chatID, 10, 64)
	if err != nil {
		return 0, coreclient.Permanent(fmt.Errorf("parse Instagram target thread %q: %w", chatID, err))
	}
	return threadKey, nil
}

// classifyThreadError dead-letters a dispatch to a thread Instagram says does
// not exist; a failed lookup is otherwise transient and retried.
func classifyThreadError(err error) error {
	if errors.Is(err, ErrThreadUnknown) {
		return coreclient.Permanent(err)
	}
	return err
}

func newOfflineThreadingID() string {
	return strconv.FormatInt(methods.GenerateEpochID(), 10)
}
