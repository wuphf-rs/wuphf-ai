// Package convert serves media.convert@1: it downloads the call's source media
// from the core, converts it with the mediaconv tools, uploads the result and
// returns its media id. Callers never see which tool ran; they name a target
// format, and the source format picks the conversion.
package convert

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"path"
	"time"

	"github.com/wuphf-rs/wuphf-ai/connectors/shared/functions"
	wuphfpb "github.com/wuphf-rs/wuphf-ai/proto/go/connector/v1"
	"github.com/wuphf-rs/wuphf-ai/providers/mediaconv/internal/mediaconv"
)

// RequestSchema describes MediaConvertArgs, the args of media.convert@1. It is
// the reference for anyone reimplementing the provider or calling it.
const RequestSchema = `{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "title": "media.convert@1 args",
  "description": "Convert the call's single source media id to target_mime_type.",
  "type": "object",
  "properties": {
    "source_mime_type": {"type": "string", "description": "MIME type of the source media, e.g. video/webm."},
    "source_file_name": {"type": "string", "description": "File name of the source media; its extension is used when the MIME type is unknown."},
    "target_mime_type": {"type": "string", "enum": ["video/webm", "image/webp"], "description": "MIME type to convert to."}
  },
  "required": ["target_mime_type"]
}`

// ResponseSchema describes MediaConvertResult, the result of media.convert@1.
const ResponseSchema = `{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "title": "media.convert@1 result",
  "description": "The converted media is the result's single media id.",
  "type": "object",
  "properties": {
    "mime_type": {"type": "string"},
    "file_name": {"type": "string"}
  },
  "required": ["mime_type", "file_name"]
}`

// MediaBroker is the core media store (coreclient.MediaClient satisfies it).
type MediaBroker interface {
	Upload(context.Context, *wuphfpb.MediaMetadata, []byte) (*wuphfpb.Attachment, error)
	Download(context.Context, string) ([]byte, error)
}

// Function returns the media.convert@1 declaration and handler.
func Function(media MediaBroker, maxConcurrency uint32, defaultDeadline time.Duration) functions.Function {
	return functions.Function{
		Name:            functions.MediaConvertName,
		Major:           functions.MediaConvertMajor,
		RequestSchema:   RequestSchema,
		ResponseSchema:  ResponseSchema,
		Idempotent:      true, // a pure conversion; running it twice is harmless
		MaxConcurrency:  maxConcurrency,
		DefaultDeadline: defaultDeadline,
		Handle:          handler{media: media}.handle,
	}
}

type handler struct {
	media MediaBroker
}

// conversion converts source bytes; ext is the source container extension.
type conversion func(ctx context.Context, data []byte, ext string) ([]byte, error)

func (h handler) handle(ctx context.Context, call functions.Call) (functions.Result, error) {
	var args functions.MediaConvertArgs
	if err := json.Unmarshal(call.Args, &args); err != nil {
		return functions.Result{}, functions.InvalidArgument("decode media.convert args: %v", err)
	}
	if args.TargetMimeType == "" {
		return functions.Result{}, functions.InvalidArgument("media.convert needs target_mime_type")
	}
	if len(call.MediaIDs) != 1 {
		return functions.Result{}, functions.InvalidArgument("media.convert takes exactly one media id")
	}
	ext := sourceExt(args.SourceMimeType, args.SourceFileName)
	convert, fileName, err := pick(ext, args.TargetMimeType)
	if err != nil {
		return functions.Result{}, err
	}

	source, err := h.media.Download(ctx, call.MediaIDs[0])
	if err != nil {
		return functions.Result{}, fmt.Errorf("download source media: %w", err)
	}
	converted, err := convert(ctx, source, ext)
	if err != nil {
		var missing *mediaconv.MissingToolError
		if errors.As(err, &missing) {
			return functions.Result{}, fmt.Errorf("convert %s to %s: %s", ext, args.TargetMimeType, missing.Error())
		}
		return functions.Result{}, fmt.Errorf("convert %s to %s: %w", ext, args.TargetMimeType, err)
	}
	size := uint64(len(converted))
	uploaded, err := h.media.Upload(ctx, &wuphfpb.MediaMetadata{
		Kind:      wuphfpb.Attachment_KIND_STICKER,
		FileName:  &fileName,
		MimeType:  &args.TargetMimeType,
		SizeBytes: &size,
		Animated:  true,
	}, converted)
	if err != nil {
		return functions.Result{}, fmt.Errorf("upload converted media: %w", err)
	}
	return functions.Result{
		Body:     functions.MediaConvertResult{MimeType: args.TargetMimeType, FileName: fileName},
		MediaIDs: []string{uploaded.GetMediaId()},
	}, nil
}

// pick chooses the conversion for a source extension and target MIME type,
// and the converted file's name.
func pick(ext, target string) (conversion, string, error) {
	switch {
	case target == "video/webm" && ext == ".webp":
		return func(ctx context.Context, data []byte, _ string) ([]byte, error) {
			return mediaconv.AnimatedWebPToWebM(ctx, data)
		}, "sticker.webm", nil
	case target == "image/webp" && ext == ".tgs":
		return func(_ context.Context, data []byte, _ string) ([]byte, error) {
			return mediaconv.TGSToAnimatedWebP(data)
		}, "sticker.webp", nil
	case target == "image/webp":
		return mediaconv.ToAnimatedWebP, "sticker.webp", nil
	default:
		return nil, "", functions.Unsupported("cannot convert %s to %s", ext, target)
	}
}

// sourceExt picks the container extension for the source bytes, from the MIME
// type with a file-name fallback, so the converter can demux them.
func sourceExt(mimeType, fileName string) string {
	switch mimeType {
	case "video/webm":
		return ".webm"
	case "application/x-tgsticker":
		return ".tgs"
	case "image/webp":
		return ".webp"
	}
	if ext := path.Ext(fileName); ext != "" {
		return ext
	}
	return ".bin"
}
