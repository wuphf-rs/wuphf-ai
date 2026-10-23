package convert

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"os"
	"os/exec"
	"path/filepath"
	"testing"

	"github.com/akshettrj/wuphf/connectors/shared/functions"
	wuphfpb "github.com/akshettrj/wuphf/proto/go/connector/v1"
)

// memoryBroker stores media in memory, recording what was uploaded.
type memoryBroker struct {
	stored   map[string][]byte
	uploaded []*wuphfpb.MediaMetadata
}

func (m *memoryBroker) Upload(_ context.Context, metadata *wuphfpb.MediaMetadata, data []byte) (*wuphfpb.Attachment, error) {
	m.uploaded = append(m.uploaded, metadata)
	id := "converted"
	m.stored[id] = data
	return &wuphfpb.Attachment{MediaId: id}, nil
}

func (m *memoryBroker) Download(_ context.Context, mediaID string) ([]byte, error) {
	data, ok := m.stored[mediaID]
	if !ok {
		return nil, errors.New("unknown media " + mediaID)
	}
	return data, nil
}

func callWith(t *testing.T, args functions.MediaConvertArgs, mediaIDs ...string) functions.Call {
	t.Helper()
	body, err := json.Marshal(args)
	if err != nil {
		t.Fatal(err)
	}
	return functions.Call{ID: "call", Args: body, MediaIDs: mediaIDs}
}

func requireCode(t *testing.T, err error, want wuphfpb.CallErrorCode) {
	t.Helper()
	var coded *functions.Error
	if !errors.As(err, &coded) || coded.Code != want {
		t.Fatalf("err = %v, want a functions.Error with %v", err, want)
	}
}

func TestHandlerRejectsInvalidArgs(t *testing.T) {
	t.Parallel()
	handle := Function(&memoryBroker{stored: map[string][]byte{}}, 1, 0).Handle

	_, err := handle(context.Background(), functions.Call{Args: []byte(`not json`), MediaIDs: []string{"source"}})
	requireCode(t, err, wuphfpb.CallErrorCode_CALL_ERROR_CODE_INVALID_ARGUMENT)
	_, err = handle(context.Background(), callWith(t, functions.MediaConvertArgs{SourceMimeType: "video/webm"}, "source"))
	requireCode(t, err, wuphfpb.CallErrorCode_CALL_ERROR_CODE_INVALID_ARGUMENT)
}

func TestHandlerRequiresExactlyOneMediaID(t *testing.T) {
	t.Parallel()
	handle := Function(&memoryBroker{stored: map[string][]byte{}}, 1, 0).Handle
	args := functions.MediaConvertArgs{SourceMimeType: "video/webm", TargetMimeType: "image/webp"}
	for _, mediaIDs := range [][]string{nil, {"a", "b"}} {
		_, err := handle(context.Background(), callWith(t, args, mediaIDs...))
		requireCode(t, err, wuphfpb.CallErrorCode_CALL_ERROR_CODE_INVALID_ARGUMENT)
	}
}

func TestHandlerRejectsUnsupportedPair(t *testing.T) {
	t.Parallel()
	handle := Function(&memoryBroker{stored: map[string][]byte{"source": []byte("x")}}, 1, 0).Handle
	for _, args := range []functions.MediaConvertArgs{
		{SourceMimeType: "video/webm", TargetMimeType: "video/webm"},
		{SourceMimeType: "image/webp", TargetMimeType: "image/png"},
	} {
		_, err := handle(context.Background(), callWith(t, args, "source"))
		requireCode(t, err, wuphfpb.CallErrorCode_CALL_ERROR_CODE_UNSUPPORTED)
	}
}

func TestFunctionDeclaresTheContract(t *testing.T) {
	t.Parallel()
	function := Function(&memoryBroker{}, 4, 0)
	if function.Name != "media.convert" || function.Major != 1 || !function.Idempotent || function.MaxConcurrency != 4 {
		t.Fatalf("function = %+v", function)
	}
	for _, schema := range []string{function.RequestSchema, function.ResponseSchema} {
		if !json.Valid([]byte(schema)) {
			t.Fatalf("schema is not valid JSON: %s", schema)
		}
	}
}

// TestHandlerConvertsWebMToAnimatedWebP runs the WebM->animated WebP path end
// to end through the handler, checking what it uploads and returns.
func TestHandlerConvertsWebMToAnimatedWebP(t *testing.T) {
	t.Parallel()
	if _, err := exec.LookPath("ffmpeg"); err != nil {
		t.Skip("ffmpeg not installed")
	}
	broker := &memoryBroker{stored: map[string][]byte{"source": buildWebM(t)}}
	result, err := Function(broker, 1, 0).Handle(context.Background(), callWith(t, functions.MediaConvertArgs{
		SourceMimeType: "video/webm",
		SourceFileName: "sticker.webm",
		TargetMimeType: "image/webp",
	}, "source"))
	if err != nil {
		t.Fatalf("handle() error = %v", err)
	}
	if len(result.MediaIDs) != 1 || result.MediaIDs[0] != "converted" {
		t.Fatalf("media ids = %v", result.MediaIDs)
	}
	if body := result.Body.(functions.MediaConvertResult); body != (functions.MediaConvertResult{MimeType: "image/webp", FileName: "sticker.webp"}) {
		t.Fatalf("body = %+v", body)
	}
	output := broker.stored["converted"]
	if !bytes.HasPrefix(output, []byte("RIFF")) || !bytes.Contains(output, []byte("ANIM")) {
		t.Fatalf("output is not an animated WebP (%d bytes)", len(output))
	}
	metadata := broker.uploaded[0]
	if metadata.GetKind() != wuphfpb.Attachment_KIND_STICKER || !metadata.GetAnimated() ||
		metadata.GetMimeType() != "image/webp" || metadata.GetFileName() != "sticker.webp" ||
		metadata.GetSizeBytes() != uint64(len(output)) {
		t.Fatalf("upload metadata = %v", metadata)
	}
}

func TestSourceExt(t *testing.T) {
	t.Parallel()
	for _, test := range []struct{ mime, name, want string }{
		{"video/webm", "x.mp4", ".webm"},
		{"application/x-tgsticker", "", ".tgs"},
		{"image/webp", "", ".webp"},
		{"video/mp4", "clip.mp4", ".mp4"},
		{"", "sticker.tgs", ".tgs"},
		{"", "", ".bin"},
	} {
		if got := sourceExt(test.mime, test.name); got != test.want {
			t.Errorf("sourceExt(%q, %q) = %q, want %q", test.mime, test.name, got, test.want)
		}
	}
}

// buildWebM renders a short VP9 clip whose frames differ, so the converted
// WebP keeps them all and is animated.
func buildWebM(t *testing.T) []byte {
	t.Helper()
	path := filepath.Join(t.TempDir(), "source.webm")
	output, err := exec.Command("ffmpeg", "-v", "error", "-f", "lavfi",
		"-i", "testsrc=size=64x64:rate=10:duration=1",
		"-c:v", "libvpx-vp9", "-pix_fmt", "yuva420p", "-y", path).CombinedOutput()
	if err != nil {
		t.Fatalf("ffmpeg: %v\n%s", err, output)
	}
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	return data
}
