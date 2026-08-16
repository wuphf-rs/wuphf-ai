package telegramconnector

import (
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
