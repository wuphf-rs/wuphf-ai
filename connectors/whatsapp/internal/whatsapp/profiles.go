package whatsappconnector

import (
	"context"
	"errors"
	"fmt"
	"io"
	"net/http"
	"strings"
	"sync"
	"time"

	wuphfpb "github.com/akshettrj/wuphf/proto/go/connector/v1"
	"github.com/rs/zerolog"
	"go.mau.fi/whatsmeow"
	"go.mau.fi/whatsmeow/types"
)

// avatarFetchTimeout bounds one picture download, so a slow CDN cannot hold
// up the event handler that asked for it for long.
const avatarFetchTimeout = 30 * time.Second

// pictureSource is the slice of the whatsmeow client that finds a chat's
// current picture.
type pictureSource interface {
	GetProfilePictureInfo(ctx context.Context, jid types.JID, params *whatsmeow.GetProfilePictureParams) (*types.ProfilePictureInfo, error)
}

// groupInfoSource gives a group's last fetched metadata (the chat namer keeps
// it alongside the title it fetched it for).
type groupInfoSource interface {
	GroupInfo(ctx context.Context, group types.JID) *types.GroupInfo
}

// fetchURL downloads a URL's body; a seam for tests.
type fetchURL func(ctx context.Context, url string) ([]byte, error)

// chatProfiler reports what a chat looks like: its picture (uploaded to the
// core's media store) and, the first time this process sees a chat, a
// snapshot of its title, description, member count and picture, so a new
// mirror topic's info card has content.
type chatProfiler struct {
	pictures pictureSource
	fetch    fetchURL
	broker   MediaBroker
	groups   groupInfoSource
	logger   zerolog.Logger

	mu   sync.Mutex
	seen map[string]bool
}

func newChatProfiler(pictures pictureSource, fetch fetchURL, broker MediaBroker, groups groupInfoSource, logger zerolog.Logger) *chatProfiler {
	return &chatProfiler{
		pictures: pictures,
		fetch:    fetch,
		broker:   broker,
		groups:   groups,
		logger:   logger,
		seen:     make(map[string]bool),
	}
}

// httpFetch is the production fetchURL: a plain GET with a 30-second limit.
func httpFetch(ctx context.Context, url string) ([]byte, error) {
	ctx, cancel := context.WithTimeout(ctx, avatarFetchTimeout)
	defer cancel()
	request, err := http.NewRequestWithContext(ctx, http.MethodGet, url, nil)
	if err != nil {
		return nil, err
	}
	response, err := http.DefaultClient.Do(request)
	if err != nil {
		return nil, err
	}
	defer response.Body.Close()
	if response.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("picture download: HTTP %d", response.StatusCode)
	}
	return io.ReadAll(response.Body)
}

// avatar is the chat's current picture: an empty id when it has none, the
// platform's picture id and the uploaded media otherwise. Nil when the
// picture could not be fetched or uploaded: the caller reports no avatar
// rather than a wrong one.
func (p *chatProfiler) avatar(ctx context.Context, jid types.JID) *wuphfpb.ChatAvatar {
	info, err := p.pictures.GetProfilePictureInfo(ctx, jid, nil)
	if errors.Is(err, whatsmeow.ErrProfilePictureNotSet) {
		return &wuphfpb.ChatAvatar{Id: ""}
	}
	if err != nil {
		p.logger.Warn().Err(err).Str("chat", jid.String()).Msg("could not look up the chat picture")
		return nil
	}
	if info == nil || info.URL == "" {
		return &wuphfpb.ChatAvatar{Id: ""}
	}
	bytes, err := p.fetch(ctx, info.URL)
	if err != nil {
		p.logger.Warn().Err(err).Str("chat", jid.String()).Msg("could not download the chat picture")
		return nil
	}
	size := uint64(len(bytes))
	attachment, err := p.broker.Upload(ctx, &wuphfpb.MediaMetadata{
		Kind:      wuphfpb.Attachment_KIND_IMAGE,
		FileName:  stringPointer("avatar.jpg"),
		MimeType:  stringPointer("image/jpeg"),
		SizeBytes: &size,
	}, bytes)
	if err != nil {
		p.logger.Warn().Err(err).Str("chat", jid.String()).Msg("could not upload the chat picture")
		return nil
	}
	mediaID := attachment.GetMediaId()
	return &wuphfpb.ChatAvatar{Id: info.ID, MediaId: &mediaID}
}

// pictureUpdate reports a picture event: a removal as an empty id, a change
// as the new picture. Nil when the new picture could not be fetched.
func (p *chatProfiler) pictureUpdate(ctx context.Context, chat types.JID, canonical string, removed bool) *wuphfpb.ChatUpdate {
	avatar := &wuphfpb.ChatAvatar{Id: ""}
	if !removed {
		avatar = p.avatar(ctx, chat)
	}
	if avatar == nil {
		return nil
	}
	return &wuphfpb.ChatUpdate{ChatId: canonical, Avatar: avatar}
}

// firstSighting returns the snapshot of a chat the first time this process
// sees it, and nil every later time. `chat` is the address the message came
// from (the one the client calls take), `canonical` the id the core keys it
// under, and `title` the display title the message carries.
func (p *chatProfiler) firstSighting(ctx context.Context, chat types.JID, canonical, title string) *wuphfpb.ChatUpdate {
	p.mu.Lock()
	if p.seen[canonical] {
		p.mu.Unlock()
		return nil
	}
	p.seen[canonical] = true
	p.mu.Unlock()

	update := &wuphfpb.ChatUpdate{ChatId: canonical}
	if title != "" {
		update.Title = &title
	}
	if chat.Server == types.GroupServer && p.groups != nil {
		if info := p.groups.GroupInfo(ctx, chat); info != nil {
			topic := info.Topic
			update.Description = &topic
			count := uint32(len(info.Participants))
			if info.ParticipantCount > 0 {
				count = uint32(info.ParticipantCount)
			}
			update.ParticipantCount = &count
		}
	}
	update.Avatar = p.avatar(ctx, chat)
	return update
}

func stringPointer(value string) *string { return &value }

// renderMentions replaces each "@<user>" in text with "@<name>", for the users
// names maps. A user without a name keeps the raw mention.
func renderMentions(text string, names map[string]string) string {
	for user, name := range names {
		if user == "" || name == "" || name == user {
			continue
		}
		text = strings.ReplaceAll(text, "@"+user, "@"+name)
	}
	return text
}
