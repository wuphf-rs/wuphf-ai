package whatsappconnector

import (
	"context"
	"fmt"

	coreclient "github.com/wuphf-rs/wuphf-ai/connectors/shared/coreclient"
	wuphfpb "github.com/wuphf-rs/wuphf-ai/proto/go/connector/v1"
	"go.mau.fi/whatsmeow"
	"go.mau.fi/whatsmeow/types"
)

// channelUpload holds the handle of a channel post's media upload (plan 085).
// A channel's media is uploaded unencrypted, and the send names the upload
// by this handle.
type channelUpload struct {
	handle string
}

// isChannel reports whether chat is a WhatsApp channel (a newsletter).
func isChannel(chat types.JID) bool {
	return chat.Server == types.NewsletterServer
}

// uploadMedia uploads one attachment: unencrypted while a channel post is
// sent, otherwise as usual.
func (s Sender) uploadMedia(ctx context.Context, data []byte, kind whatsmeow.MediaType) (whatsmeow.UploadResponse, error) {
	if s.channel == nil {
		return s.client.Upload(ctx, data, kind)
	}
	uploaded, err := s.client.UploadNewsletter(ctx, data, kind)
	if err == nil {
		s.channel.handle = uploaded.Handle
	}
	return uploaded, err
}

// postToChannel posts the dispatch to a channel the owner owns or admins
// (plan 085): its text, or each attachment with the text as the first one's
// caption. It quotes nothing and carries no timer. Anything but an explicit
// post is refused: a channel post reaches every follower.
func (s Sender) postToChannel(ctx context.Context, dispatch *wuphfpb.MessageCreateDispatch, chat types.JID) (string, error) {
	if !dispatch.GetPost() {
		return "", coreclient.Permanent(fmt.Errorf("refusing to send to channel %s: a channel post goes out only on purpose (/post)", chat))
	}
	if len(dispatch.Attachments) == 0 {
		if dispatch.GetText() == "" {
			return "", coreclient.Permanent(fmt.Errorf("an empty channel post cannot be posted"))
		}
		response, err := s.client.SendMessage(ctx, chat, textMessage(dispatch.GetText()))
		if err != nil {
			return "", fmt.Errorf("post to WhatsApp channel: %w", err)
		}
		return string(response.ID), nil
	}
	var first string
	for index, attachment := range dispatch.Attachments {
		caption := ""
		if index == 0 {
			caption = dispatch.GetText()
		}
		upload := &channelUpload{}
		channel := s
		channel.channel = upload
		message, err := channel.attachmentMessage(ctx, attachment, caption, false)
		if err != nil {
			return "", attachmentGroupError(err, index)
		}
		response, err := s.client.SendMessage(ctx, chat, message, whatsmeow.SendRequestExtra{MediaHandle: upload.handle})
		if err != nil {
			return "", attachmentGroupError(fmt.Errorf("post media to WhatsApp channel: %w", err), index)
		}
		if index == 0 {
			first = string(response.ID)
		}
	}
	return first, nil
}
