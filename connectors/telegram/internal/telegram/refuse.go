package telegramconnector

import (
	"context"
	"fmt"
	"strconv"
	"time"

	"github.com/gotd/td/telegram/message/styling"
	coreclient "github.com/wuphf-rs/wuphf-ai/connectors/shared/coreclient"
	wuphfpb "github.com/wuphf-rs/wuphf-ai/proto/go/connector/v1"
)

// refusedMark is the reaction on a message the bridge did not send on.
// Telegram's standard reaction set (all a bot or a non-Premium session may
// use) has no 🚫.
const refusedMark = "👎"

// RefuseMessage surfaces that the bridge did not send a message on (plan
// 083: a message typed into a receive-only chat's topic, a story quoted
// outside its poster's chat). The message gets a 👎 that stays, and the
// reason is posted as a reply note, deleted after the dispatch's TTL; a
// restart before then leaves the note. The mark is best effort (a chat can
// turn reactions off): the note is what must arrive. The target id is
// returned unchanged.
func (s Sender) RefuseMessage(ctx context.Context, dispatch *wuphfpb.MessageRefusedDispatch) (string, error) {
	peer, err := s.peers.InputPeer(ctx, dispatch.TargetChatId)
	if err != nil {
		return "", err
	}
	messageID, err := strconv.Atoi(dispatch.TargetPlatformMessageId)
	if err != nil {
		return "", coreclient.Permanent(fmt.Errorf("invalid Telegram target message id %q: %w", dispatch.TargetPlatformMessageId, err))
	}
	if _, err := s.react(ctx, dispatch.TargetChatId, dispatch.TargetPlatformMessageId, refusedMark); err != nil {
		s.peers.logger.Warn().Err(err).Str("chat", dispatch.TargetChatId).Msg("could not mark a refused message")
	}
	updates, err := s.sender.To(peer).CloneBuilder().Reply(messageID).StyledText(ctx, styling.Italic(dispatch.GetReason()))
	if err != nil {
		return "", fmt.Errorf("post Telegram refusal note: %w", err)
	}
	if ttl := dispatch.GetNoteTtlSecs(); ttl > 0 {
		if noteID, err := sentMessageID(updates); err == nil {
			s.afterFunc()(time.Duration(ttl)*time.Second, func() {
				deleteCtx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
				defer cancel()
				if err := s.deleteMessages(deleteCtx, peer, noteID); err != nil {
					s.peers.logger.Warn().Err(err).Str("chat", dispatch.TargetChatId).Msg("could not delete a refusal note")
				}
			})
		}
	}
	return dispatch.TargetPlatformMessageId, nil
}

// afterFunc is the sender's timer: time.AfterFunc unless a test replaced it.
func (s Sender) afterFunc() func(time.Duration, func()) {
	if s.after != nil {
		return s.after
	}
	return func(d time.Duration, f func()) { time.AfterFunc(d, f) }
}
