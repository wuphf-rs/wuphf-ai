package telegramconnector

import (
	"context"
	"errors"
	"fmt"
	"unicode/utf16"

	"github.com/gotd/td/telegram/message"
	"github.com/gotd/td/telegram/message/styling"
	"github.com/gotd/td/tg"
	wuphfpb "github.com/wuphf-rs/wuphf-ai/proto/go/connector/v1"
)

// Edits that no longer fit as text (plan 086). A copy can be plain text, text
// sent as a file because it was too long (spillFileName), or media with a
// caption. Telegram edits each differently and cannot turn media back into
// text, so the sender reads the copy back first and keeps its message id:
//
//   - text that still fits is edited; text that no longer fits becomes the
//     file, in place;
//   - a spilled copy gets a new file with the edited text, however short;
//   - a caption that fits is edited; one past captionLimit is cut short, and
//     the full text follows as a file in a reply.

// spillFileName names the file over-long text is sent as.
const spillFileName = "message.txt"

// spillMime is the spilled file's type.
const spillMime = "text/plain; charset=utf-8"

// captionLimit is the longest caption Telegram takes from any account
// (Premium accounts get more), in UTF-16 code units.
const captionLimit = 1024

// copyKind is how a mirrored copy was rendered.
type copyKind int

const (
	copyText copyKind = iota
	copySpilled
	copyMedia
)

// kindOf tells how msg was rendered.
func kindOf(msg *tg.Message) copyKind {
	media, ok := msg.GetMedia()
	if !ok {
		return copyText
	}
	document, ok := media.(*tg.MessageMediaDocument)
	if !ok {
		return copyMedia
	}
	file, ok := document.Document.(*tg.Document)
	if !ok {
		return copyMedia
	}
	for _, attribute := range file.Attributes {
		if name, ok := attribute.(*tg.DocumentAttributeFilename); ok && name.FileName == spillFileName {
			return copySpilled
		}
	}
	return copyMedia
}

// fetchCopy reads one of this chat's messages back.
func (s Sender) fetchCopy(ctx context.Context, peer tg.InputPeerClass, id int) (*tg.Message, error) {
	ids := []tg.InputMessageClass{&tg.InputMessageID{ID: id}}
	var result tg.MessagesMessagesClass
	var err error
	if channel, ok := peer.(*tg.InputPeerChannel); ok {
		result, err = s.api.ChannelsGetMessages(ctx, &tg.ChannelsGetMessagesRequest{
			Channel: &tg.InputChannel{ChannelID: channel.ChannelID, AccessHash: channel.AccessHash},
			ID:      ids,
		})
	} else {
		result, err = s.api.MessagesGetMessages(ctx, ids)
	}
	if err != nil {
		return nil, fmt.Errorf("read the Telegram copy back: %w", err)
	}
	modified, ok := result.AsModified()
	if !ok {
		return nil, errors.New("Telegram returned no messages")
	}
	for _, candidate := range modified.GetMessages() {
		if msg, ok := candidate.(*tg.Message); ok && msg.ID == id {
			return msg, nil
		}
	}
	return nil, errors.New("the Telegram copy is gone")
}

// editInPlace revises copy id with the dispatch's text the way its rendering
// takes it. A copy that cannot be read back is edited as text, as before.
func (s Sender) editInPlace(ctx context.Context, peer tg.InputPeerClass, id int, dispatch *wuphfpb.MessageEditDispatch) error {
	name, participant, text := dispatch.GetSenderName(), dispatch.GetParticipant(), dispatch.GetText()
	kind := copyText
	if msg, err := s.fetchCopy(ctx, peer, id); err == nil {
		kind = kindOf(msg)
	} else {
		s.peers.logger.Debug().Err(err).Msg("editing a Telegram copy as text")
	}
	switch kind {
	case copySpilled:
		return s.replaceWithFile(ctx, peer, id, name, participant, text)
	case copyMedia:
		return s.editCaption(ctx, peer, id, name, participant, text)
	default:
		if textExceedsLimit(text, s.rendering().MaxMessageLength) {
			return s.replaceWithFile(ctx, peer, id, name, participant, text)
		}
		if _, err := s.sender.To(peer).Edit(id).StyledText(ctx, attributedText(name, participant, text)...); err != nil {
			return fmt.Errorf("edit Telegram message: %w", err)
		}
		return nil
	}
}

// replaceWithFile swaps copy id's content for the edited text as a file.
func (s Sender) replaceWithFile(ctx context.Context, peer tg.InputPeerClass, id int, name, participant, text string) error {
	file, err := s.uploadText(ctx, peer, plainAttributedText(name, participant, text))
	if err != nil {
		return err
	}
	if _, err := s.sender.To(peer).Edit(id).Media(ctx, namedFile(file, spillFileName, spillMime)); err != nil {
		return fmt.Errorf("replace the Telegram text file: %w", err)
	}
	return nil
}

// editCaption revises a media copy's caption; one too long for a caption is
// cut short, and the full text follows in a reply as a file.
func (s Sender) editCaption(ctx context.Context, peer tg.InputPeerClass, id int, name, participant, text string) error {
	full := plainAttributedText(name, participant, text)
	if utf16Len(full) <= captionLimit {
		if _, err := s.sender.To(peer).Edit(id).StyledText(ctx, attributedText(name, participant, text)...); err != nil {
			return fmt.Errorf("edit Telegram caption: %w", err)
		}
		return nil
	}
	label := senderLabel(name, participant)
	room := captionLimit - utf16Len(label) - utf16Len("\n\n") - utf16Len("…")
	short := attributedText(name, participant, cutUTF16(text, room)+"…")
	if label == "" {
		short = []message.StyledTextOption{styling.Plain(cutUTF16(text, captionLimit-1) + "…")}
	}
	if _, err := s.sender.To(peer).Edit(id).StyledText(ctx, short...); err != nil {
		return fmt.Errorf("edit Telegram caption: %w", err)
	}
	file, err := s.uploadText(ctx, peer, full)
	if err != nil {
		return err
	}
	if _, err := s.sender.To(peer).CloneBuilder().Reply(id).Media(ctx, namedFile(file, spillFileName, spillMime)); err != nil {
		return fmt.Errorf("send the full Telegram caption: %w", err)
	}
	return nil
}

// uploadText uploads content as the spill file.
func (s Sender) uploadText(ctx context.Context, peer tg.InputPeerClass, content string) (tg.InputFileClass, error) {
	file, err := s.sender.To(peer).CloneBuilder().Upload(message.FromBytes(spillFileName, []byte(content))).AsInputFile(ctx)
	if err != nil {
		return nil, fmt.Errorf("upload Telegram text file: %w", err)
	}
	return file, nil
}

func utf16Len(text string) int {
	return len(utf16.Encode([]rune(text)))
}

// cutUTF16 is the longest prefix of text at most limit UTF-16 code units
// long, never splitting a character.
func cutUTF16(text string, limit int) string {
	if limit <= 0 {
		return ""
	}
	units := 0
	for index, r := range text {
		width := 1
		if r >= 0x10000 {
			width = 2
		}
		if units+width > limit {
			return text[:index]
		}
		units += width
	}
	return text
}
