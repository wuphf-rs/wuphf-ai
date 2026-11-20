package instagramconnector

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"time"

	"github.com/akshettrj/wuphf/connectors/shared/functions"
	wuphfpb "github.com/akshettrj/wuphf/proto/go/connector/v1"
	"github.com/rs/zerolog"
	"go.mau.fi/mautrix-meta/pkg/messagix/responses"
)

// FetchMediaRequestSchema describes functions.InstagramFetchMediaArgs.
const FetchMediaRequestSchema = `{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "title": "instagram.fetch_media@1 args",
  "description": "Download the media of an Instagram post or reel link.",
  "type": "object",
  "properties": {
    "url": {"type": "string", "description": "An instagram.com /p/, /reel/ or /reels/ link."}
  },
  "required": ["url"]
}`

// FetchMediaResponseSchema describes functions.InstagramFetchMediaResult.
const FetchMediaResponseSchema = `{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "title": "instagram.fetch_media@1 result",
  "description": "The downloaded media are the result's media ids, in items order.",
  "type": "object",
  "properties": {
    "items": {"type": "array", "items": {"type": "object", "properties": {"kind": {"enum": ["video", "image"]}}, "required": ["kind"]}},
    "author": {"type": "string", "description": "The poster's username."},
    "caption": {"type": "string", "description": "The post's text."},
    "skipped": {"type": "integer", "description": "Media left out: too large, or a failed download."}
  },
  "required": ["items"]
}`

// fetchMediaDeadline is the deadline the core gives a call that names none: a
// carousel of videos takes several downloads.
const fetchMediaDeadline = 60 * time.Second

// FetchMediaFunction returns the instagram.fetch_media@1 declaration and
// handler. It runs on the connector's logged-in session (codec's fetcher) and
// stores the media through codec's broker.
func FetchMediaFunction(codec MediaCodec, maxConcurrency uint32, logger zerolog.Logger) functions.Function {
	return functions.Function{
		Name:            functions.InstagramFetchMediaName,
		Major:           functions.InstagramFetchMediaMajor,
		RequestSchema:   FetchMediaRequestSchema,
		ResponseSchema:  FetchMediaResponseSchema,
		Idempotent:      true, // a read; running it twice only uploads twice
		MaxConcurrency:  maxConcurrency,
		DefaultDeadline: fetchMediaDeadline,
		Handle:          fetchMediaHandler{codec: codec, logger: logger}.handle,
	}
}

type fetchMediaHandler struct {
	codec  MediaCodec
	logger zerolog.Logger
}

func (h fetchMediaHandler) handle(ctx context.Context, call functions.Call) (functions.Result, error) {
	var args functions.InstagramFetchMediaArgs
	if err := json.Unmarshal(call.Args, &args); err != nil {
		return functions.Result{}, functions.InvalidArgument("decode %s args: %v", functions.InstagramFetchMediaName, err)
	}
	shortcode, ok := functions.ParseInstagramPostLink(args.URL)
	if !ok {
		return functions.Result{}, functions.InvalidArgument("%q is not an Instagram post or reel link", args.URL)
	}
	mediaID, err := MediaIDFromShortcode(shortcode)
	if err != nil {
		return functions.Result{}, functions.InvalidArgument("%v", err)
	}
	if h.codec.fetcher == nil {
		return functions.Result{}, errors.New("no Instagram session to fetch media with")
	}
	response, err := h.codec.fetcher.FetchMedia(ctx, mediaID, shortcode)
	if err != nil {
		return functions.Result{}, fmt.Errorf("fetch post %s: %w", shortcode, err)
	}
	if response == nil || len(response.Items) == 0 || response.Items[0] == nil {
		return functions.Result{}, fmt.Errorf("fetch post %s: Instagram returned no media", shortcode)
	}
	post := response.Items[0]
	parts := post.CarouselMedia
	if len(parts) == 0 {
		parts = []*responses.Items{post}
	}

	result := functions.InstagramFetchMediaResult{
		Items:   []functions.InstagramMediaItem{},
		Author:  post.User.Username,
		Caption: post.Caption.Text,
	}
	var mediaIDs []string
	for index, part := range parts {
		attachment, kind, err := h.download(ctx, part)
		if err != nil {
			if ctx.Err() != nil {
				return functions.Result{}, ctx.Err()
			}
			h.logger.Warn().Err(err).Str("shortcode", shortcode).Int("item", index).Msg("Instagram post item skipped")
			result.Skipped++
			continue
		}
		result.Items = append(result.Items, functions.InstagramMediaItem{Kind: kind})
		mediaIDs = append(mediaIDs, attachment.GetMediaId())
	}
	if len(mediaIDs) == 0 {
		return functions.Result{}, fmt.Errorf("fetch post %s: none of its %d media could be downloaded", shortcode, len(parts))
	}
	return functions.Result{Body: result, MediaIDs: mediaIDs}, nil
}

// download stores one item's best rendition in the core. A body over the
// download cap or the core's upload limit fails here.
func (h fetchMediaHandler) download(ctx context.Context, item *responses.Items) (*wuphfpb.Attachment, string, error) {
	if item == nil {
		return nil, "", errors.New("empty item")
	}
	url, kind := bestRendition(item)
	if url == "" {
		return nil, "", errors.New("item has no video or image")
	}
	attachment, err := h.codec.fetch(ctx, mediaRef{metadata: &wuphfpb.MediaMetadata{Kind: kind}, url: url})
	if err != nil {
		return nil, "", err
	}
	if kind == wuphfpb.Attachment_KIND_VIDEO {
		return attachment, functions.InstagramMediaVideo, nil
	}
	return attachment, functions.InstagramMediaImage, nil
}
