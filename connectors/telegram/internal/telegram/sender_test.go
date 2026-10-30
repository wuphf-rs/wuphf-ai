package telegramconnector

import (
	"context"
	"errors"
	"slices"
	"testing"

	"github.com/akshettrj/wuphf/proto/go/connector/v1"
	"github.com/gotd/td/tg"
)

func TestAttributedText(t *testing.T) {
	t.Parallel()

	cases := []struct {
		name        string
		senderName  string
		participant string
		text        string
		wantOptions int
	}{
		{"group message gets bold name plus body", "Alice", "p1", "hi", 2},
		{"group message falls back to participant id", "", "p1", "hi", 2},
		{"group header only when body empty", "Alice", "p1", "", 1},
		{"direct message left unlabelled", "Alice", "", "hi", 1},
		{"nothing to send yields no options", "", "", "", 0},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			if got := attributedText(tc.senderName, tc.participant, tc.text); len(got) != tc.wantOptions {
				t.Fatalf("attributedText() = %d options, want %d", len(got), tc.wantOptions)
			}
		})
	}
}

func TestUploadName(t *testing.T) {
	t.Parallel()

	cases := []struct {
		name            string
		attachment      *wuphfpb.Attachment
		want            string
		wantExtensionOK bool
	}{
		{
			"image without a filename gets a mime extension",
			&wuphfpb.Attachment{Kind: wuphfpb.Attachment_KIND_IMAGE, MimeType: ptr("image/jpeg")},
			"attachment.jpg", true,
		},
		{
			"mime parameters are ignored",
			&wuphfpb.Attachment{Kind: wuphfpb.Attachment_KIND_AUDIO, MimeType: ptr("audio/ogg; codecs=opus")},
			"attachment.ogg", true,
		},
		{
			"unknown mime yields no extension",
			&wuphfpb.Attachment{Kind: wuphfpb.Attachment_KIND_IMAGE, MimeType: ptr("image/heif")},
			"attachment", false,
		},
		{
			"existing extension is preserved",
			&wuphfpb.Attachment{Kind: wuphfpb.Attachment_KIND_FILE, FileName: ptr("report.pdf"), MimeType: ptr("application/pdf")},
			"report.pdf", true,
		},
		{
			"named file without extension gets one from mime",
			&wuphfpb.Attachment{Kind: wuphfpb.Attachment_KIND_IMAGE, FileName: ptr("photo"), MimeType: ptr("image/png")},
			"photo.png", true,
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			got, ok := uploadName(tc.attachment)
			if got != tc.want || ok != tc.wantExtensionOK {
				t.Fatalf("uploadName() = (%q, %t), want (%q, %t)", got, ok, tc.want, tc.wantExtensionOK)
			}
		})
	}
}

func ptr(s string) *string { return &s }

func TestTextExceedsLimit(t *testing.T) {
	t.Parallel()

	cases := []struct {
		name  string
		text  string
		limit uint32
		want  bool
	}{
		{"zero limit is unbounded", "anything goes here", 0, false},
		{"within limit", "hello", 5, false},
		{"over limit", "hello!", 5, true},
		{"astral chars count as two utf-16 units", "😀😀😀", 5, true},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			if got := textExceedsLimit(tc.text, tc.limit); got != tc.want {
				t.Fatalf("textExceedsLimit(%q, %d) = %t, want %t", tc.text, tc.limit, got, tc.want)
			}
		})
	}
}

func TestPlainAttributedText(t *testing.T) {
	t.Parallel()

	cases := []struct {
		name        string
		senderName  string
		participant string
		text        string
		want        string
	}{
		{"direct message left unlabelled", "Alice", "", "hi", "hi"},
		{"group message gets name header", "Alice", "p1", "hi", "🗣️: Alice\n\nhi"},
		{"group header falls back to participant id", "", "p1", "hi", "🗣️: p1\n\nhi"},
		{"group header only when body empty", "Alice", "p1", "", "🗣️: Alice"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			if got := plainAttributedText(tc.senderName, tc.participant, tc.text); got != tc.want {
				t.Fatalf("plainAttributedText() = %q, want %q", got, tc.want)
			}
		})
	}
}

func TestSentMessageID(t *testing.T) {
	t.Parallel()

	id, err := sentMessageID(&tg.Updates{
		Updates: []tg.UpdateClass{
			&tg.UpdateNewChannelMessage{Message: &tg.Message{ID: 42}},
		},
	})
	if err != nil {
		t.Fatalf("sentMessageID() error = %v", err)
	}
	if id != 42 {
		t.Fatalf("sentMessageID() = %d, want 42", id)
	}
}

func TestAlbumGrouping(t *testing.T) {
	t.Parallel()

	image := func() *wuphfpb.Attachment {
		return &wuphfpb.Attachment{Kind: wuphfpb.Attachment_KIND_IMAGE, MimeType: ptr("image/jpeg")}
	}
	video := func() *wuphfpb.Attachment {
		return &wuphfpb.Attachment{Kind: wuphfpb.Attachment_KIND_VIDEO, MimeType: ptr("video/mp4")}
	}
	audio := func() *wuphfpb.Attachment {
		return &wuphfpb.Attachment{Kind: wuphfpb.Attachment_KIND_AUDIO, MimeType: ptr("audio/mpeg")}
	}

	cases := []struct {
		name        string
		attachments []*wuphfpb.Attachment
		groupable   bool
	}{
		{"photos group", []*wuphfpb.Attachment{image(), image()}, true},
		{"photos and videos share the visual group", []*wuphfpb.Attachment{image(), video()}, true},
		{"audio does not group with photos", []*wuphfpb.Attachment{image(), audio()}, false},
		{
			"a sticker never groups",
			[]*wuphfpb.Attachment{image(), {Kind: wuphfpb.Attachment_KIND_STICKER, MimeType: ptr("image/webp")}},
			false,
		},
		{
			"an animation never groups",
			[]*wuphfpb.Attachment{video(), {Kind: wuphfpb.Attachment_KIND_VIDEO, MimeType: ptr("video/mp4"), Animated: true}},
			false,
		},
		{
			"an extensionless item degrades to a document and stops grouping with photos",
			[]*wuphfpb.Attachment{image(), {Kind: wuphfpb.Attachment_KIND_IMAGE}},
			false,
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			if got := groupable(tc.attachments); got != tc.groupable {
				t.Fatalf("groupable() = %v, want %v", got, tc.groupable)
			}
		})
	}
}

// fakeConverter records each conversion request and answers with fixed bytes
// or an error.
type fakeConverter struct {
	calls  []convertCall
	output []byte
	err    error
}

type convertCall struct {
	mediaID, sourceMime, sourceName, targetMime string
}

func (f *fakeConverter) Convert(_ context.Context, mediaID, sourceMime, sourceName, targetMime string) ([]byte, error) {
	f.calls = append(f.calls, convertCall{mediaID, sourceMime, sourceName, targetMime})
	return f.output, f.err
}

func animatedSticker(mimeType, fileName string) *wuphfpb.Attachment {
	return &wuphfpb.Attachment{
		MediaId:  "media-1",
		Kind:     wuphfpb.Attachment_KIND_STICKER,
		MimeType: &mimeType,
		FileName: &fileName,
		Animated: true,
	}
}

func TestAnimatedStickerWebMPassesWebMThrough(t *testing.T) {
	t.Parallel()

	converter := &fakeConverter{}
	sender := Sender{converter: converter}
	got, err := sender.animatedStickerWebM(context.Background(), animatedSticker("video/webm", "sticker.webm"), []byte("webm"))
	if err != nil {
		t.Fatal(err)
	}
	if string(got) != "webm" {
		t.Errorf("bytes = %q, want the source unchanged", got)
	}
	if len(converter.calls) != 0 {
		t.Errorf("converter called %d times, want 0", len(converter.calls))
	}
}

func TestAnimatedStickerWebMConvertsWebP(t *testing.T) {
	t.Parallel()

	converter := &fakeConverter{output: []byte("converted webm")}
	sender := Sender{converter: converter}
	got, err := sender.animatedStickerWebM(context.Background(), animatedSticker("image/webp", "sticker.webp"), []byte("webp"))
	if err != nil {
		t.Fatal(err)
	}
	if string(got) != "converted webm" {
		t.Errorf("bytes = %q, want the converter's output", got)
	}
	want := []convertCall{{"media-1", "image/webp", "sticker.webp", "video/webm"}}
	if !slices.Equal(converter.calls, want) {
		t.Errorf("converter calls = %+v, want %+v", converter.calls, want)
	}
}

func TestAnimatedStickerWebMReturnsConverterError(t *testing.T) {
	t.Parallel()

	failure := errors.New("no provider for media.convert@1 connected")
	sender := Sender{converter: &fakeConverter{err: failure}}
	if _, err := sender.animatedStickerWebM(context.Background(), animatedSticker("image/webp", "sticker.webp"), []byte("webp")); !errors.Is(err, failure) {
		t.Errorf("err = %v, want the converter's error", err)
	}
}

func TestAFileKeepsItsNameAndType(t *testing.T) {
	api := &recordingAPI{}
	mime := "application/pdf"
	dispatch := &wuphfpb.MessageCreateDispatch{
		DispatchId:     "d-1",
		TargetChatId:   "telegram:chat:42",
		TargetThreadId: ptr("7"),
		Participant:    ptr("p1"),
		Attachments: []*wuphfpb.Attachment{{
			MediaId:  "m",
			Kind:     wuphfpb.Attachment_KIND_FILE,
			MimeType: &mime,
			FileName: ptr("file.pdf"),
		}},
	}
	if _, _, err := noteSender(api).SendMessage(context.Background(), dispatch); err != nil {
		t.Fatal(err)
	}
	sends := sent[*tg.MessagesSendMediaRequest](api)
	if len(sends) != 1 {
		t.Fatalf("got %d media sends, want 1", len(sends))
	}
	document, ok := sends[0].Media.(*tg.InputMediaUploadedDocument)
	if !ok {
		t.Fatalf("media = %T, want an uploaded document", sends[0].Media)
	}
	if document.MimeType != mime {
		t.Fatalf("mime type = %q, want %q", document.MimeType, mime)
	}
	var named string
	for _, attribute := range document.Attributes {
		if filename, ok := attribute.(*tg.DocumentAttributeFilename); ok {
			named = filename.FileName
		}
	}
	if named != "file.pdf" {
		t.Fatalf("file name = %q, want %q", named, "file.pdf")
	}
}
