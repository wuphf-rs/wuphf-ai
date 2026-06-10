package instagramconnector

import (
	"context"
	"fmt"
	"io"
	"net/http"
	"strconv"
	"strings"

	wuphfpb "github.com/akshettrj/wuphf/proto/go/connector/v1"
	"go.mau.fi/mautrix-meta/pkg/messagix/data/responses"
	"go.mau.fi/mautrix-meta/pkg/messagix/table"
)

// MediaBroker stores inbound media bytes in the core and returns the
// core-owned attachment handle dispatches refer to.
type MediaBroker interface {
	Upload(context.Context, *wuphfpb.MediaMetadata, []byte) (*wuphfpb.Attachment, error)
}

// HTTPDoer fetches a media URL. Abstracted so tests can stub the CDN.
type HTTPDoer interface {
	Do(*http.Request) (*http.Response, error)
}

// MediaFetcher resolves a shared post or reel to its full-quality media. The
// lightspeed table only carries a low-resolution preview for link-style shares,
// so the original media is fetched from the media API by id and shortcode.
// *messagix.InstagramMethods satisfies this.
type MediaFetcher interface {
	FetchMedia(ctx context.Context, mediaID, shortcode string) (*responses.FetchMediaResponse, error)
}

// MediaCodec downloads Instagram CDN media and stores it in the core. Instagram
// exposes media only as signed CDN URLs in the lightspeed table, so inbound
// media is fetched over HTTP rather than through a platform download API.
type MediaCodec struct {
	broker  MediaBroker
	http    HTTPDoer
	fetcher MediaFetcher
}

func NewMediaCodec(broker MediaBroker, doer HTTPDoer, fetcher MediaFetcher) MediaCodec {
	return MediaCodec{broker: broker, http: doer, fetcher: fetcher}
}

// mediaRef is one downloadable attachment discovered in a delta, tied to the
// message it belongs to by the platform message id.
type mediaRef struct {
	messageID string
	metadata  *wuphfpb.MediaMetadata
	url       string
	// share, when set, identifies a link-style share (reel or post) whose
	// full-quality media is fetched from the media API; url is the low-res
	// table preview used as a fallback when the fetch fails.
	share *shareMedia
}

// shareMedia identifies a shared post or reel for a full-quality media fetch.
type shareMedia struct {
	mediaID   string
	shortcode string
}

// fetchAll downloads every ref and stores it in the core, preserving order so an
// album's attachments keep their original sequence.
func (m MediaCodec) fetchAll(ctx context.Context, refs []mediaRef) ([]*wuphfpb.Attachment, error) {
	attachments := make([]*wuphfpb.Attachment, 0, len(refs))
	for _, ref := range refs {
		attachment, err := m.fetch(ctx, ref)
		if err != nil {
			return nil, err
		}
		attachments = append(attachments, attachment)
	}
	return attachments, nil
}

func (m MediaCodec) fetch(ctx context.Context, ref mediaRef) (*wuphfpb.Attachment, error) {
	url := ref.url
	if ref.share != nil {
		if best := m.fetchShareURL(ctx, ref.share); best != "" {
			url = best
		}
	}
	request, err := http.NewRequestWithContext(ctx, http.MethodGet, url, nil)
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
	bytes, err := io.ReadAll(response.Body)
	if err != nil {
		return nil, fmt.Errorf("read Instagram media body: %w", err)
	}
	attachment, err := m.broker.Upload(ctx, ref.metadata, bytes)
	if err != nil {
		return nil, fmt.Errorf("store Instagram media in core: %w", err)
	}
	return attachment, nil
}

// fetchShareURL resolves a shared post or reel to the URL of its largest
// available rendition. Quality is best-effort: any failure returns "" and the
// caller falls back to the table's preview URL rather than losing the message.
func (m MediaCodec) fetchShareURL(ctx context.Context, share *shareMedia) string {
	if m.fetcher == nil {
		return ""
	}
	response, err := m.fetcher.FetchMedia(ctx, share.mediaID, share.shortcode)
	if err != nil || len(response.Items) == 0 || response.Items[0] == nil {
		return ""
	}
	item := response.Items[0]
	best, bestArea := "", 0
	for _, version := range item.VideoVersions {
		if area := version.Width * version.Height; version.URL != "" && area > bestArea {
			best, bestArea = version.URL, area
		}
	}
	if best != "" {
		return best
	}
	for _, candidate := range item.ImageVersions2.Candidates {
		if area := candidate.Width * candidate.Height; candidate.URL != "" && area > bestArea {
			best, bestArea = candidate.URL, area
		}
	}
	return best
}

// mediaBundle holds everything an attachment delta contributes to one message:
// downloadable attachments plus a caption suffix for link-style XMA shares
// (e.g. reels) whose body lives in the share metadata rather than the message.
type mediaBundle struct {
	attachments []mediaRef
	suffix      string
}

// collectMedia groups a delta's attachment rows by their message id. It performs
// no I/O; the normalizer downloads the refs it returns.
func collectMedia(tbl *table.LSTable) map[string]*mediaBundle {
	bundles := map[string]*mediaBundle{}
	bundle := func(messageID string) *mediaBundle {
		existing, ok := bundles[messageID]
		if !ok {
			existing = &mediaBundle{}
			bundles[messageID] = existing
		}
		return existing
	}
	for _, blob := range tbl.LSInsertBlobAttachment {
		if ref, ok := blobMediaRef(blob); ok {
			b := bundle(ref.messageID)
			b.attachments = append(b.attachments, ref)
		}
	}
	for _, sticker := range tbl.LSInsertStickerAttachment {
		if ref, ok := stickerMediaRef(sticker); ok {
			b := bundle(ref.messageID)
			b.attachments = append(b.attachments, ref)
		}
	}
	ctas := map[string]*table.LSInsertAttachmentCta{}
	for _, cta := range tbl.LSInsertAttachmentCta {
		if cta.AttachmentFbid != "" {
			ctas[cta.AttachmentFbid] = cta
		}
	}
	for _, xma := range tbl.LSInsertXmaAttachment {
		ref, suffix, ok := xmaMediaRef(xma, ctas[xma.AttachmentFbid])
		if !ok {
			continue
		}
		b := bundle(xma.MessageId)
		if ref.url != "" {
			b.attachments = append(b.attachments, ref)
		}
		b.suffix = appendLine(b.suffix, suffix)
	}
	return bundles
}

func blobMediaRef(blob *table.LSInsertBlobAttachment) (mediaRef, bool) {
	url := firstNonEmpty(blob.PlayableUrl, blob.PreviewUrl)
	if blob.MessageId == "" || url == "" {
		return mediaRef{}, false
	}
	return mediaRef{
		messageID: blob.MessageId,
		metadata: &wuphfpb.MediaMetadata{
			Kind:      blobKind(blob.AttachmentType),
			FileName:  optionalString(blob.Filename),
			MimeType:  optionalString(firstNonEmpty(blob.AttachmentMimeType, blob.PlayableUrlMimeType)),
			SizeBytes: optionalSize(blob.Filesize),
		},
		url: url,
	}, true
}

func stickerMediaRef(sticker *table.LSInsertStickerAttachment) (mediaRef, bool) {
	url := firstNonEmpty(sticker.PlayableUrl, sticker.ImageUrl, sticker.PreviewUrl)
	if sticker.MessageId == "" || url == "" {
		return mediaRef{}, false
	}
	return mediaRef{
		messageID: sticker.MessageId,
		metadata: &wuphfpb.MediaMetadata{
			Kind:     wuphfpb.Attachment_KIND_STICKER,
			MimeType: optionalString(firstNonEmpty(sticker.PlayableUrlMimeType, sticker.ImageUrlMimeType, sticker.PreviewUrlMimeType)),
		},
		url: url,
	}, true
}

// xmaMediaRef handles link-style shares such as reels and shared posts. A reel
// or shared video exposes a playable video URL, which is downloaded so the share
// bridges as the actual video rather than only its still preview; other shares
// fall back to the preview image. The share's title/source and a public link
// (derived from its CTA, when present) are returned as a caption suffix to
// append to the message text.
func xmaMediaRef(xma *table.LSInsertXmaAttachment, cta *table.LSInsertAttachmentCta) (mediaRef, string, bool) {
	if xma.MessageId == "" {
		return mediaRef{}, "", false
	}
	title := firstNonEmpty(xma.TitleText, xma.DescriptionText)
	link := firstNonEmpty(shareURL(cta), xma.ActionUrl, xma.SourceText)
	suffix := appendLine(appendLine(title, xma.SubtitleText), link)
	share := shareRef(cta)
	ref := mediaRef{}
	if video := firstNonEmpty(xma.PlayableUrl, xma.PlayableUrlFallback); video != "" {
		ref = mediaRef{
			messageID: xma.MessageId,
			metadata: &wuphfpb.MediaMetadata{
				Kind:     wuphfpb.Attachment_KIND_VIDEO,
				MimeType: optionalString(xma.PlayableUrlMimeType),
			},
			url:   video,
			share: share,
		}
	} else if preview := firstNonEmpty(xma.ImageUrl, xma.PreviewUrl); preview != "" {
		ref = mediaRef{
			messageID: xma.MessageId,
			metadata: &wuphfpb.MediaMetadata{
				Kind:     wuphfpb.Attachment_KIND_IMAGE,
				MimeType: optionalString(xma.PreviewUrlMimeType),
			},
			url:   preview,
			share: share,
		}
	}
	if ref.url == "" && suffix == "" {
		return mediaRef{}, "", false
	}
	return ref, suffix, true
}

// Shortcode deep-link prefixes used by post and reel share CTAs.
const (
	postPrefix = "instagram://media/?shortcode="
	reelPrefix = "instagram://reels_share/?shortcode="
)

// shareURL derives the public Instagram link for a shared post, reel, or story
// from its CTA. Reels and posts carry a shortcode in the CTA's native deep link;
// stories carry a /stories/direct path in the action url; other CTAs may already
// hold a web url. Returns "" when no public link can be built.
func shareURL(cta *table.LSInsertAttachmentCta) string {
	if cta == nil {
		return ""
	}
	switch {
	case strings.HasPrefix(cta.NativeUrl, postPrefix):
		return "https://www.instagram.com/p/" + strings.TrimPrefix(cta.NativeUrl, postPrefix) + "/"
	case strings.HasPrefix(cta.NativeUrl, reelPrefix):
		return "https://www.instagram.com/reel/" + strings.TrimPrefix(cta.NativeUrl, reelPrefix) + "/"
	case strings.HasPrefix(cta.ActionUrl, "/stories/direct/"):
		return "https://www.instagram.com" + cta.ActionUrl
	case strings.HasPrefix(cta.ActionUrl, "http"):
		return cta.ActionUrl
	}
	return ""
}

// shareRef extracts the media id and shortcode a full-quality fetch needs from a
// share CTA. Only post and reel shares carry both; returns nil for everything
// else, which keeps the table preview URL as the media source.
func shareRef(cta *table.LSInsertAttachmentCta) *shareMedia {
	if cta == nil || cta.TargetId == 0 {
		return nil
	}
	shortcode := ""
	switch {
	case strings.HasPrefix(cta.NativeUrl, postPrefix):
		shortcode = strings.TrimPrefix(cta.NativeUrl, postPrefix)
	case strings.HasPrefix(cta.NativeUrl, reelPrefix):
		shortcode = strings.TrimPrefix(cta.NativeUrl, reelPrefix)
	}
	if shortcode == "" {
		return nil
	}
	return &shareMedia{
		mediaID:   strconv.FormatInt(cta.TargetId, 10),
		shortcode: shortcode,
	}
}

// blobKind maps an Instagram attachment type to a core attachment kind,
// defaulting to a generic file for anything without a media-type analogue.
func blobKind(kind table.AttachmentType) wuphfpb.Attachment_Kind {
	switch kind {
	case table.AttachmentTypeSticker, table.AttachmentTypeSelfieSticker, table.AttachmentTypeThirdPartySticker:
		return wuphfpb.Attachment_KIND_STICKER
	case table.AttachmentTypeImage, table.AttachmentTypeAnimatedImage, table.AttachmentTypeEphemeralImage:
		return wuphfpb.Attachment_KIND_IMAGE
	case table.AttachmentTypeVideo, table.AttachmentTypeEphemeralVideo:
		return wuphfpb.Attachment_KIND_VIDEO
	case table.AttachmentTypeAudio, table.AttachmentTypeSoundBite:
		return wuphfpb.Attachment_KIND_AUDIO
	default:
		return wuphfpb.Attachment_KIND_FILE
	}
}

func firstNonEmpty(values ...string) string {
	for _, value := range values {
		if value != "" {
			return value
		}
	}
	return ""
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

func optionalSize(value int64) *uint64 {
	if value <= 0 {
		return nil
	}
	size := uint64(value)
	return &size
}
