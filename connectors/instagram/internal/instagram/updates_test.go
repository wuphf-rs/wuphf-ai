package instagramconnector

import (
	"context"
	"io"
	"net/http"
	"strconv"
	"strings"
	"testing"

	wuphfpb "github.com/akshettrj/wuphf/proto/go/connector/v1"
	"github.com/rs/zerolog"
	"go.mau.fi/mautrix-meta/pkg/messagix/data/responses"
	"go.mau.fi/mautrix-meta/pkg/messagix/table"
)

type stubBroker struct{ uploaded [][]byte }

func (s *stubBroker) Upload(_ context.Context, meta *wuphfpb.MediaMetadata, data []byte) (*wuphfpb.Attachment, error) {
	s.uploaded = append(s.uploaded, data)
	return &wuphfpb.Attachment{
		MediaId:  "media-" + strconv.Itoa(len(s.uploaded)),
		Kind:     meta.GetKind(),
		MimeType: meta.MimeType,
		FileName: meta.FileName,
	}, nil
}

type stubDoer struct {
	body     string
	requests []string
}

func (s *stubDoer) Do(req *http.Request) (*http.Response, error) {
	s.requests = append(s.requests, req.URL.String())
	return &http.Response{
		StatusCode: http.StatusOK,
		Status:     "200 OK",
		Body:       io.NopCloser(strings.NewReader(s.body)),
	}, nil
}

type stubFetcher struct {
	resp *responses.FetchMediaResponse
	err  error
}

func (s stubFetcher) FetchMedia(context.Context, string, string) (*responses.FetchMediaResponse, error) {
	return s.resp, s.err
}

func newTestNormalizer(broker MediaBroker, doer HTTPDoer) (*Normalizer, chan *wuphfpb.ClientFrame) {
	ch := make(chan *wuphfpb.ClientFrame, 16)
	return NewNormalizer(ch, NewMediaCodec(broker, doer, nil), zerolog.Nop()), ch
}

func drain(ch <-chan *wuphfpb.ClientFrame) []*wuphfpb.ClientFrame {
	var frames []*wuphfpb.ClientFrame
	for {
		select {
		case f := <-ch:
			frames = append(frames, f)
		default:
			return frames
		}
	}
}

func TestHandleCreateCarriesGroupTitle(t *testing.T) {
	t.Parallel()
	n, ch := newTestNormalizer(&stubBroker{}, &stubDoer{})
	err := n.Handle(context.Background(), &table.LSTable{
		LSDeleteThenInsertThread: []*table.LSDeleteThenInsertThread{{ThreadKey: 42, ThreadName: "Squad"}},
		LSInsertMessage: []*table.LSInsertMessage{{
			Text: "hello", ThreadKey: 42, SenderId: 7, MessageId: "m1",
			TimestampMs: 1700000000000, ReplySourceId: "m0",
		}},
	})
	if err != nil {
		t.Fatalf("Handle() error = %v", err)
	}
	frames := drain(ch)
	if len(frames) != 1 {
		t.Fatalf("frames = %d, want 1", len(frames))
	}
	event := frames[0].GetMessageCreate()
	if event == nil {
		t.Fatalf("frame = %#v, want create", frames[0])
	}
	if event.ChatId != "42" || event.GetChatTitle() != "Squad" {
		t.Fatalf("chat = %q title = %q", event.ChatId, event.GetChatTitle())
	}
	if event.GetText() != "hello" || event.GetReplyToPlatformMessageId() != "m0" {
		t.Fatalf("text = %q reply = %q", event.GetText(), event.GetReplyToPlatformMessageId())
	}
}

func TestHandleCreateTitleFromContactForDM(t *testing.T) {
	t.Parallel()
	n, ch := newTestNormalizer(&stubBroker{}, &stubDoer{})
	err := n.Handle(context.Background(), &table.LSTable{
		LSVerifyContactRowExists: []*table.LSVerifyContactRowExists{{ContactId: 99, Name: "Alice"}},
		LSInsertMessage:          []*table.LSInsertMessage{{Text: "hi", ThreadKey: 99, SenderId: 99, MessageId: "m1"}},
	})
	if err != nil {
		t.Fatalf("Handle() error = %v", err)
	}
	frames := drain(ch)
	if len(frames) != 1 || frames[0].GetMessageCreate().GetChatTitle() != "Alice" {
		t.Fatalf("frames = %#v, want DM titled Alice", frames)
	}
}

func TestHandleUnsentInsertEmitsDelete(t *testing.T) {
	t.Parallel()
	n, ch := newTestNormalizer(&stubBroker{}, &stubDoer{})
	err := n.Handle(context.Background(), &table.LSTable{
		LSInsertMessage: []*table.LSInsertMessage{{ThreadKey: 42, MessageId: "m1", IsUnsent: true}},
	})
	if err != nil {
		t.Fatalf("Handle() error = %v", err)
	}
	frames := drain(ch)
	del := frames[0].GetMessageDelete()
	if len(frames) != 1 || del == nil || del.ChatId != "42" || del.PlatformMessageId != "m1" {
		t.Fatalf("frames = %#v, want delete for m1", frames)
	}
}

func TestHandleDeleteRow(t *testing.T) {
	t.Parallel()
	n, ch := newTestNormalizer(&stubBroker{}, &stubDoer{})
	err := n.Handle(context.Background(), &table.LSTable{
		LSDeleteMessage: []*table.LSDeleteMessage{{ThreadKey: 42, MessageId: "m1"}},
	})
	if err != nil {
		t.Fatalf("Handle() error = %v", err)
	}
	frames := drain(ch)
	if len(frames) != 1 || frames[0].GetMessageDelete().GetChatId() != "42" {
		t.Fatalf("frames = %#v, want delete", frames)
	}
}

func TestHandleEditResolvesThreadFromCache(t *testing.T) {
	t.Parallel()
	n, ch := newTestNormalizer(&stubBroker{}, &stubDoer{})
	if err := n.Handle(context.Background(), &table.LSTable{
		LSInsertMessage: []*table.LSInsertMessage{{Text: "v1", ThreadKey: 42, MessageId: "m1"}},
	}); err != nil {
		t.Fatalf("seed Handle() error = %v", err)
	}
	if err := n.Handle(context.Background(), &table.LSTable{
		LSEditMessage: []*table.LSEditMessage{{MessageID: "m1", Text: "v2"}},
	}); err != nil {
		t.Fatalf("edit Handle() error = %v", err)
	}
	frames := drain(ch)
	if len(frames) != 2 {
		t.Fatalf("frames = %d, want 2", len(frames))
	}
	edit := frames[1].GetMessageEdit()
	if edit == nil || edit.ChatId != "42" || edit.GetText() != "v2" {
		t.Fatalf("edit = %#v", edit)
	}
}

func TestHandleEditSkipsUnknownMessage(t *testing.T) {
	t.Parallel()
	n, ch := newTestNormalizer(&stubBroker{}, &stubDoer{})
	err := n.Handle(context.Background(), &table.LSTable{
		LSEditMessage: []*table.LSEditMessage{{MessageID: "ghost", Text: "v2"}},
	})
	if err != nil {
		t.Fatalf("Handle() error = %v", err)
	}
	if frames := drain(ch); len(frames) != 0 {
		t.Fatalf("frames = %#v, want none", frames)
	}
}

func TestHandleCreateDownloadsAttachment(t *testing.T) {
	t.Parallel()
	broker := &stubBroker{}
	n, ch := newTestNormalizer(broker, &stubDoer{body: "imgdata"})
	err := n.Handle(context.Background(), &table.LSTable{
		LSInsertMessage: []*table.LSInsertMessage{{ThreadKey: 42, MessageId: "m1"}},
		LSInsertBlobAttachment: []*table.LSInsertBlobAttachment{{
			MessageId: "m1", PlayableUrl: "https://cdn/x.jpg",
			AttachmentType: table.AttachmentTypeImage, AttachmentMimeType: "image/jpeg",
		}},
	})
	if err != nil {
		t.Fatalf("Handle() error = %v", err)
	}
	frames := drain(ch)
	event := frames[0].GetMessageCreate()
	if len(frames) != 1 || event == nil || len(event.Attachments) != 1 {
		t.Fatalf("frames = %#v, want one create with one attachment", frames)
	}
	if event.Attachments[0].GetKind() != wuphfpb.Attachment_KIND_IMAGE {
		t.Fatalf("attachment kind = %v", event.Attachments[0].GetKind())
	}
	if string(broker.uploaded[0]) != "imgdata" {
		t.Fatalf("uploaded = %q", broker.uploaded[0])
	}
}

func TestCollectMediaGroupsByMessage(t *testing.T) {
	t.Parallel()
	bundles := collectMedia(&table.LSTable{
		LSInsertBlobAttachment: []*table.LSInsertBlobAttachment{
			{MessageId: "m1", PlayableUrl: "u1", AttachmentType: table.AttachmentTypeImage},
			{MessageId: "m1", PlayableUrl: "u2", AttachmentType: table.AttachmentTypeVideo},
		},
		LSInsertXmaAttachment: []*table.LSInsertXmaAttachment{
			{MessageId: "m1", TitleText: "Reel", ActionUrl: "https://reel"},
		},
	})
	bundle := bundles["m1"]
	if bundle == nil || len(bundle.attachments) != 2 {
		t.Fatalf("bundle = %#v, want two attachments", bundle)
	}
	if bundle.suffix != "Reel\nhttps://reel" {
		t.Fatalf("suffix = %q", bundle.suffix)
	}
}

func TestCollectMediaReelDownloadsVideo(t *testing.T) {
	t.Parallel()
	bundles := collectMedia(&table.LSTable{
		LSInsertXmaAttachment: []*table.LSInsertXmaAttachment{
			{
				MessageId:           "m1",
				AttachmentFbid:      "fb1",
				PlayableUrl:         "https://cdn/reel.mp4",
				PlayableUrlMimeType: "video/mp4",
				ImageUrl:            "https://cdn/reel-thumb.jpg",
				TitleText:           "Reel",
			},
		},
		LSInsertAttachmentCta: []*table.LSInsertAttachmentCta{
			{AttachmentFbid: "fb1", NativeUrl: "instagram://reels_share/?shortcode=ABC123"},
		},
	})
	bundle := bundles["m1"]
	if bundle == nil || len(bundle.attachments) != 1 {
		t.Fatalf("bundle = %#v, want one attachment", bundle)
	}
	ref := bundle.attachments[0]
	if ref.url != "https://cdn/reel.mp4" {
		t.Fatalf("url = %q, want the playable video url", ref.url)
	}
	if ref.metadata.Kind != wuphfpb.Attachment_KIND_VIDEO {
		t.Fatalf("kind = %v, want KIND_VIDEO", ref.metadata.Kind)
	}
	if bundle.suffix != "Reel\nhttps://www.instagram.com/reel/ABC123/" {
		t.Fatalf("suffix = %q", bundle.suffix)
	}
}

func TestCollectMediaShareCarriesFetchRef(t *testing.T) {
	t.Parallel()
	bundles := collectMedia(&table.LSTable{
		LSInsertXmaAttachment: []*table.LSInsertXmaAttachment{
			{MessageId: "m1", AttachmentFbid: "fb1", PlayableUrl: "https://cdn/preview.mp4"},
		},
		LSInsertAttachmentCta: []*table.LSInsertAttachmentCta{
			{AttachmentFbid: "fb1", TargetId: 555, NativeUrl: "instagram://reels_share/?shortcode=ABC123"},
		},
	})
	share := bundles["m1"].attachments[0].share
	if share == nil || share.mediaID != "555" || share.shortcode != "ABC123" {
		t.Fatalf("share = %#v, want media 555 shortcode ABC123", share)
	}
}

func TestFetchSharePrefersLargestVersion(t *testing.T) {
	t.Parallel()
	doer := &stubDoer{body: "video"}
	codec := NewMediaCodec(&stubBroker{}, doer, stubFetcher{resp: &responses.FetchMediaResponse{
		Items: []*responses.Items{{
			VideoVersions: []responses.VideoVersions{
				{Width: 480, Height: 854, URL: "https://cdn/low.mp4"},
				{Width: 1080, Height: 1920, URL: "https://cdn/full.mp4"},
			},
		}},
	}})
	ref := mediaRef{
		messageID: "m1",
		metadata:  &wuphfpb.MediaMetadata{Kind: wuphfpb.Attachment_KIND_VIDEO},
		url:       "https://cdn/preview.mp4",
		share:     &shareMedia{mediaID: "555", shortcode: "ABC123"},
	}
	if _, err := codec.fetch(context.Background(), ref); err != nil {
		t.Fatalf("fetch() error = %v", err)
	}
	if len(doer.requests) != 1 || doer.requests[0] != "https://cdn/full.mp4" {
		t.Fatalf("requests = %v, want the largest video version", doer.requests)
	}
}

func TestFetchShareFallsBackToPreviewOnError(t *testing.T) {
	t.Parallel()
	doer := &stubDoer{body: "video"}
	codec := NewMediaCodec(&stubBroker{}, doer, stubFetcher{err: context.DeadlineExceeded})
	ref := mediaRef{
		messageID: "m1",
		metadata:  &wuphfpb.MediaMetadata{Kind: wuphfpb.Attachment_KIND_VIDEO},
		url:       "https://cdn/preview.mp4",
		share:     &shareMedia{mediaID: "555", shortcode: "ABC123"},
	}
	if _, err := codec.fetch(context.Background(), ref); err != nil {
		t.Fatalf("fetch() error = %v", err)
	}
	if len(doer.requests) != 1 || doer.requests[0] != "https://cdn/preview.mp4" {
		t.Fatalf("requests = %v, want the preview fallback", doer.requests)
	}
}

func TestBlobKindMapping(t *testing.T) {
	t.Parallel()
	cases := map[table.AttachmentType]wuphfpb.Attachment_Kind{
		table.AttachmentTypeImage:   wuphfpb.Attachment_KIND_IMAGE,
		table.AttachmentTypeVideo:   wuphfpb.Attachment_KIND_VIDEO,
		table.AttachmentTypeAudio:   wuphfpb.Attachment_KIND_AUDIO,
		table.AttachmentTypeSticker: wuphfpb.Attachment_KIND_STICKER,
		table.AttachmentTypeFile:    wuphfpb.Attachment_KIND_FILE,
	}
	for in, want := range cases {
		if got := blobKind(in); got != want {
			t.Fatalf("blobKind(%v) = %v, want %v", in, got, want)
		}
	}
}
