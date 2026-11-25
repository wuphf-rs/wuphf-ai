package instagramconnector

import (
	"cmp"
	"context"
	"fmt"
	"io"
	"net/http"
	"strings"

	wuphfpb "github.com/wuphf-rs/wuphf-ai/proto/go/connector/v1"
	"go.mau.fi/mautrix-meta/pkg/instameow/slidetypes"
	"go.mau.fi/mautrix-meta/pkg/messagix/responses"
)

// maxDownloadBytes caps a CDN media download. The core rejects anything over
// its configured max_media_bytes at upload anyway (32 MiB by default); this
// bound only prevents buffering an unbounded response body before that check.
const maxDownloadBytes = 64 * 1024 * 1024

// MediaBroker stores inbound media bytes in the core and returns the
// core-owned attachment handle dispatches refer to.
type MediaBroker interface {
	Upload(context.Context, *wuphfpb.MediaMetadata, []byte) (*wuphfpb.Attachment, error)
}

// HTTPDoer fetches a media URL. Abstracted so tests can stub the CDN.
type HTTPDoer interface {
	Do(*http.Request) (*http.Response, error)
}

// MediaFetcher resolves a shared post or reel to its full-quality media. A
// share message only carries a low-resolution preview, so the original media
// is fetched from the media API by id and shortcode. *instameow.Client
// satisfies this.
type MediaFetcher interface {
	FetchMedia(ctx context.Context, mediaID, shortcode string) (*responses.FetchMediaResponse, error)
}

// MediaCodec downloads Instagram CDN media and stores it in the core. Instagram
// exposes media only as signed CDN URLs on the message, so inbound media is
// fetched over HTTP rather than through a platform download API.
type MediaCodec struct {
	broker  MediaBroker
	http    HTTPDoer
	fetcher MediaFetcher
}

func NewMediaCodec(broker MediaBroker, doer HTTPDoer, fetcher MediaFetcher) MediaCodec {
	return MediaCodec{broker: broker, http: doer, fetcher: fetcher}
}

// mediaRef is one downloadable attachment of a message.
type mediaRef struct {
	metadata *wuphfpb.MediaMetadata
	url      string
	// share, when set, identifies a shared post or reel whose full-quality
	// media is fetched from the media API; url is the preview used as a
	// fallback when the fetch fails.
	share *shareMedia
}

// shareMedia identifies a shared post or reel for a full-quality media fetch.
type shareMedia struct {
	mediaID string
	// shortcode is "" when the share named its media only by id.
	shortcode string
	// wantsLink is set when the share carried no link of its own: the
	// fetched item's link is then added to the message text.
	wantsLink bool
}

// fetchedMedia is what downloading a message's media produced: the stored
// attachments, in order, and the post or reel links learned while resolving
// shares that carried none.
type fetchedMedia struct {
	attachments []*wuphfpb.Attachment
	links       []string
}

// fetchAll downloads every ref and stores it in the core, preserving order so an
// album's attachments keep their original sequence. On a failed download the
// links learned so far are still returned, so the text keeps them.
func (m MediaCodec) fetchAll(ctx context.Context, refs []mediaRef) (fetchedMedia, error) {
	fetched := fetchedMedia{attachments: make([]*wuphfpb.Attachment, 0, len(refs))}
	for _, ref := range refs {
		ref, link := m.resolveShare(ctx, ref)
		if link != "" {
			fetched.links = append(fetched.links, link)
		}
		attachment, err := m.fetch(ctx, ref)
		if err != nil {
			return fetchedMedia{links: fetched.links}, err
		}
		fetched.attachments = append(fetched.attachments, attachment)
	}
	return fetched, nil
}

// resolveShare points a shared post or reel's ref at its full-quality media.
// It returns the item's link when the share wants one. Quality is
// best-effort: when the fetch fails the ref keeps its preview.
func (m MediaCodec) resolveShare(ctx context.Context, ref mediaRef) (mediaRef, string) {
	if ref.share == nil || m.fetcher == nil {
		return ref, ""
	}
	response, err := m.fetcher.FetchMedia(ctx, ref.share.mediaID, ref.share.shortcode)
	if err != nil || response == nil || len(response.Items) == 0 || response.Items[0] == nil {
		return ref, ""
	}
	item := response.Items[0]
	if best, kind := bestRendition(item); best != "" {
		ref.url = best
		ref.metadata.Kind = kind
	}
	if !ref.share.wantsLink {
		return ref, ""
	}
	return ref, permalink(item)
}

// fetch downloads one ref's URL and stores the bytes in the core.
func (m MediaCodec) fetch(ctx context.Context, ref mediaRef) (*wuphfpb.Attachment, error) {
	request, err := http.NewRequestWithContext(ctx, http.MethodGet, ref.url, nil)
	if err != nil {
		return nil, fmt.Errorf("build Instagram media request: %w", err)
	}
	response, err := m.http.Do(request)
	if err != nil {
		return nil, fmt.Errorf("download Instagram media: %w", err)
	}
	defer response.Body.Close()
	if response.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("download Instagram media: unexpected status %s", response.Status)
	}
	bytes, err := io.ReadAll(io.LimitReader(response.Body, maxDownloadBytes+1))
	if err != nil {
		return nil, fmt.Errorf("read Instagram media body: %w", err)
	}
	if len(bytes) > maxDownloadBytes {
		return nil, fmt.Errorf("download Instagram media: body exceeds %d bytes", maxDownloadBytes)
	}
	if ref.metadata.MimeType == nil {
		ref.metadata.MimeType = optionalString(mimeBase(response.Header.Get("Content-Type")))
	}
	attachment, err := m.broker.Upload(ctx, ref.metadata, bytes)
	if err != nil {
		return nil, fmt.Errorf("store Instagram media in core: %w", err)
	}
	return attachment, nil
}

// bestRendition picks an item's largest video, else its largest image.
func bestRendition(item *responses.Items) (string, wuphfpb.Attachment_Kind) {
	best, bestArea := "", 0
	for _, version := range item.VideoVersions {
		if area := version.Width * version.Height; version.URL != "" && area > bestArea {
			best, bestArea = version.URL, area
		}
	}
	if best != "" {
		return best, wuphfpb.Attachment_KIND_VIDEO
	}
	for _, candidate := range item.ImageVersions2.Candidates {
		if area := candidate.Width * candidate.Height; candidate.URL != "" && area > bestArea {
			best, bestArea = candidate.URL, area
		}
	}
	return best, wuphfpb.Attachment_KIND_IMAGE
}

// messageBody is what one Instagram message contributes to a create event:
// its text and the media to download. It performs no I/O.
type messageBody struct {
	text  string
	media []mediaRef
	// unresolvedShare is a share with neither a target link nor anything
	// found to fetch; it is logged so a new share shape can be diagnosed.
	unresolvedShare *slidetypes.XMAContent
}

// viewOnceNotice stands in for view-once media Instagram withholds from the
// web client, so the mirror still shows that something was sent.
const viewOnceNotice = "[view-once media: open Instagram to view]"

// bodyOf maps an Instagram message's content to text and media refs. Admin
// lines that only announce a reaction are dropped: the reaction itself is
// bridged as a reaction.
func bodyOf(message *slidetypes.Message) messageBody {
	switch content := message.Content.Content.(type) {
	case *slidetypes.MessageContentText:
		return messageBody{text: content.TextBody}
	case *slidetypes.MessageContentAdminText:
		if content.IsReactionActionLog {
			return messageBody{}
		}
		return messageBody{text: cmp.Or(adminText(content.TextFragments), message.IGDSnippet)}
	case *slidetypes.MessageContentImage:
		return messageBody{media: attachmentRefs(content.Attachments, wuphfpb.Attachment_KIND_IMAGE)}
	case *slidetypes.MessageContentVideo:
		return messageBody{media: attachmentRefs(content.Videos, wuphfpb.Attachment_KIND_VIDEO)}
	case *slidetypes.MessageContentMultiMedia:
		return messageBody{media: attachmentRefs(content.Attachments, 0)}
	case *slidetypes.MessageContentRavenImage:
		return ravenBody(content.Attachment, wuphfpb.Attachment_KIND_IMAGE)
	case *slidetypes.MessageContentRavenVideo:
		return ravenBody(content.Attachment, wuphfpb.Attachment_KIND_VIDEO)
	case *slidetypes.MessageContentAudio:
		var refs []mediaRef
		for _, audio := range content.AudioAttachments {
			if audio != nil && audio.AttachmentCDNURL != "" {
				refs = append(refs, mediaRef{
					metadata: &wuphfpb.MediaMetadata{Kind: wuphfpb.Attachment_KIND_AUDIO},
					url:      audio.AttachmentCDNURL,
				})
			}
		}
		return messageBody{media: refs}
	case *slidetypes.MessageContentAnimatedMedia:
		var refs []mediaRef
		for _, animated := range content.AnimatedMedia {
			if ref, ok := animatedRef(animated); ok {
				refs = append(refs, ref)
			}
		}
		return messageBody{media: refs}
	case *slidetypes.MessageContentSticker:
		if content.PreviewURL == "" {
			return messageBody{text: content.AltText}
		}
		return messageBody{media: []mediaRef{{
			metadata: &wuphfpb.MediaMetadata{Kind: wuphfpb.Attachment_KIND_STICKER},
			url:      content.PreviewURL,
		}}}
	case *slidetypes.MessageContentMusicSticker:
		track := content.AudioTrack
		body := messageBody{text: joinNonEmpty(" — ", track.Title, track.DisplayArtist)}
		if track.Web30SPreviewDownloadURL != "" {
			body.media = []mediaRef{{
				metadata: &wuphfpb.MediaMetadata{Kind: wuphfpb.Attachment_KIND_AUDIO},
				url:      track.Web30SPreviewDownloadURL,
			}}
		}
		return body
	case *slidetypes.MessageContentXMA:
		return xmaBody(content)
	case *slidetypes.MessageContentAIRichResponse:
		return messageBody{text: content.UnifiedResponse}
	case *slidetypes.MessageContentAISearchResponse:
		return messageBody{text: content.MessageTextBody}
	default:
		// Unknown content still has Instagram's own one-line summary.
		return messageBody{text: message.IGDSnippet}
	}
}

func ravenBody(attachment *slidetypes.Attachment, kind wuphfpb.Attachment_Kind) messageBody {
	refs := attachmentRefs([]*slidetypes.Attachment{attachment}, kind)
	if len(refs) == 0 {
		return messageBody{text: viewOnceNotice}
	}
	return messageBody{media: refs}
}

// attachmentRefs maps photo/video attachments to refs. kind 0 means the list
// mixes photos and videos (an album), so each is classified by its own shape.
func attachmentRefs(attachments []*slidetypes.Attachment, kind wuphfpb.Attachment_Kind) []mediaRef {
	var refs []mediaRef
	for _, attachment := range attachments {
		if attachment == nil {
			continue
		}
		mediaURL := cmp.Or(attachment.AttachmentCDNURL, attachment.AttachmentCDNFallbackURL, attachment.PreviewCDNURL)
		if mediaURL == "" {
			continue
		}
		refKind := kind
		if refKind == 0 {
			refKind = wuphfpb.Attachment_KIND_IMAGE
			if attachment.DashManifest != "" || strings.Contains(attachment.Typename, "Video") {
				refKind = wuphfpb.Attachment_KIND_VIDEO
			}
		}
		refs = append(refs, mediaRef{
			metadata: &wuphfpb.MediaMetadata{Kind: refKind},
			url:      mediaURL,
		})
	}
	return refs
}

// animatedRef picks a GIF or animated sticker's best rendition: the webp for
// stickers, the mp4 for GIFs, else the still preview.
func animatedRef(animated *slidetypes.AnimatedAttachment) (mediaRef, bool) {
	switch {
	case animated == nil:
		return mediaRef{}, false
	case animated.AttachmentWebpURL != "" && (animated.IsSticker || animated.AttachmentMP4URL == ""):
		return mediaRef{
			metadata: &wuphfpb.MediaMetadata{Kind: wuphfpb.Attachment_KIND_STICKER, MimeType: optionalString("image/webp")},
			url:      animated.AttachmentWebpURL,
		}, true
	case animated.AttachmentMP4URL != "":
		return mediaRef{
			metadata: &wuphfpb.MediaMetadata{Kind: wuphfpb.Attachment_KIND_VIDEO, MimeType: optionalString("video/mp4")},
			url:      animated.AttachmentMP4URL,
		}, true
	case animated.PreviewCDNURL != "":
		return mediaRef{
			metadata: &wuphfpb.MediaMetadata{Kind: wuphfpb.Attachment_KIND_IMAGE},
			url:      animated.PreviewCDNURL,
		}, true
	}
	return mediaRef{}, false
}

// xmaBody handles link-style shares (posts, reels, stories, links, forwarded
// comments). The text the sender typed comes first, then the share's title,
// subtitle and public link. A post or reel is bridged as its media, resolved
// to full quality when possible; a link only as text plus its preview image. A
// forwarded comment names no post, so it bridges as the comment, a note that
// the post is only on Instagram, and the post's cover.
func xmaBody(content *slidetypes.MessageContentXMA) messageBody {
	xma := content.XMA
	if xma == nil {
		return messageBody{text: content.XMATextBody}
	}
	share := shareRef(xma)
	var body messageBody
	if comment, ok := forwardedCommentText(xma); ok {
		body.text = appendLine(content.XMATextBody, comment)
	} else {
		text := appendLine(content.XMATextBody, cmp.Or(xma.TitleText, xma.XMATitle, xma.CaptionBodyText))
		text = appendLine(text, xma.SubtitleText)
		body.text = appendLine(text, xma.TargetURL)
		if share == nil && xma.TargetURL == "" {
			body.unresolvedShare = xma
		}
	}
	preview := xma.PreviewImage
	if preview == nil {
		preview = xma.XMAPreviewImage
	}
	if preview == nil || cmp.Or(preview.URL, preview.FallbackURL) == "" {
		return body
	}
	body.media = []mediaRef{{
		metadata: &wuphfpb.MediaMetadata{Kind: wuphfpb.Attachment_KIND_IMAGE},
		url:      cmp.Or(preview.URL, preview.FallbackURL),
		share:    share,
	}}
	return body
}

func adminText(fragments []slidetypes.TextFragment) string {
	var text strings.Builder
	for _, fragment := range fragments {
		text.WriteString(fragment.Plaintext)
	}
	return strings.TrimSpace(text.String())
}

// mimeBase strips parameters from a Content-Type header value.
func mimeBase(contentType string) string {
	base, _, _ := strings.Cut(contentType, ";")
	return strings.TrimSpace(base)
}

func joinNonEmpty(separator string, values ...string) string {
	var kept []string
	for _, value := range values {
		if value != "" {
			kept = append(kept, value)
		}
	}
	return strings.Join(kept, separator)
}

func appendLine(base, addition string) string {
	if addition == "" {
		return base
	}
	if base == "" {
		return addition
	}
	return base + "\n" + addition
}

func optionalString(value string) *string {
	if value == "" {
		return nil
	}
	return &value
}
