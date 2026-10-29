// The media.convert@1 contract, shared by its callers (connectors converting
// stickers) and its provider (providers/mediaconv), so both sides encode the
// same bodies.

package functions

import (
	"context"
	"fmt"
	"time"
)

const (
	MediaConvertName  = "media.convert"
	MediaConvertMajor = 1
)

// MediaConvertArgs are the args of media.convert@1. The source media is the
// call's single media id.
type MediaConvertArgs struct {
	SourceMimeType string `json:"source_mime_type,omitempty"`
	SourceFileName string `json:"source_file_name,omitempty"`
	TargetMimeType string `json:"target_mime_type"`
}

// MediaConvertResult is the result of media.convert@1. The converted media is
// the result's single media id.
type MediaConvertResult struct {
	MimeType string `json:"mime_type"`
	FileName string `json:"file_name"`
}

// MediaDownloader fetches stored media by id (coreclient.MediaClient satisfies it).
type MediaDownloader interface {
	Download(ctx context.Context, mediaID string) ([]byte, error)
}

// MediaConverter converts stored media through media.convert@1 and returns the
// converted bytes.
type MediaConverter struct {
	Functions *Client
	Media     MediaDownloader
	Timeout   time.Duration // bounds the whole call, including waiting for a provider
	// TimeoutFunc, when set, is read on every call instead of Timeout, so a
	// timeout that is a runtime setting applies to the next conversion.
	TimeoutFunc func() time.Duration
}

// Convert converts the stored media mediaID to targetMime. sourceMime and
// sourceName describe the source so the provider can pick a conversion.
func (m MediaConverter) Convert(ctx context.Context, mediaID, sourceMime, sourceName, targetMime string) ([]byte, error) {
	timeout := m.Timeout
	if m.TimeoutFunc != nil {
		timeout = m.TimeoutFunc()
	}
	if timeout > 0 {
		var cancel context.CancelFunc
		ctx, cancel = context.WithTimeout(ctx, timeout)
		defer cancel()
	}
	var result MediaConvertResult
	mediaIDs, err := m.Functions.Invoke(ctx, MediaConvertName, MediaConvertMajor, MediaConvertArgs{
		SourceMimeType: sourceMime,
		SourceFileName: sourceName,
		TargetMimeType: targetMime,
	}, []string{mediaID}, &result)
	if err != nil {
		return nil, err
	}
	if len(mediaIDs) != 1 {
		return nil, fmt.Errorf("%s@%d returned %d media ids, want 1", MediaConvertName, MediaConvertMajor, len(mediaIDs))
	}
	data, err := m.Media.Download(ctx, mediaIDs[0])
	if err != nil {
		return nil, fmt.Errorf("download converted media: %w", err)
	}
	return data, nil
}
