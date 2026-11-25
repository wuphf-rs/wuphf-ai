package instagramconnector

import (
	"context"
	"encoding/json"
	"strings"
	"testing"

	"github.com/rs/zerolog"
	wuphfpb "github.com/wuphf-rs/wuphf-ai/proto/go/connector/v1"
	"go.mau.fi/mautrix-meta/pkg/instameow/slidetypes"
	"go.mau.fi/mautrix-meta/pkg/messagix/responses"
)

// decodeXMA wraps one XMA object in share content.
func decodeXMA(t *testing.T, raw string) *slidetypes.MessageContentXMA {
	t.Helper()
	var xma slidetypes.XMAContent
	if err := json.Unmarshal([]byte(raw), &xma); err != nil {
		t.Fatalf("decode xma: %v", err)
	}
	return &slidetypes.MessageContentXMA{XMA: &xma}
}

func TestParseShareLink(t *testing.T) {
	t.Parallel()
	cases := []struct {
		raw       string
		kind      string
		shortcode string
		ok        bool
	}{
		{"https://www.instagram.com/reel/DgKBiN1vLsH/?igsh=x", "reel", "DgKBiN1vLsH", true},
		{"https://instagram.com/reels/DgKBiN1vLsH", "reel", "DgKBiN1vLsH", true},
		{"https://m.instagram.com/p/Bxyz-_1/", "p", "Bxyz-_1", true},
		{"https://www.instagram.com/tv/Bxyz/", "tv", "Bxyz", true},
		{"https://www.instagram.com/some.user/reel/DgKBiN1vLsH/", "reel", "DgKBiN1vLsH", true},
		{"https://www.instagram.com/reel/DgKBiN1vLsH/c/18012345678901234/", "reel", "DgKBiN1vLsH", true},
		{"https://www.instagram.com/stories/someone/3012345678901234567/", "", "", false},
		{"https://www.instagram.com/someone/", "", "", false},
		{"https://example.com/reel/DgKBiN1vLsH/", "", "", false},
		{"instagram://media?id=1", "", "", false},
		{"", "", "", false},
	}
	for _, tc := range cases {
		link, ok := parseShareLink(tc.raw)
		if ok != tc.ok || link.kind != tc.kind || link.shortcode != tc.shortcode {
			t.Errorf("parseShareLink(%q) = %+v, %v; want %s/%s, %v", tc.raw, link, ok, tc.kind, tc.shortcode, tc.ok)
		}
	}
}

func TestPermalink(t *testing.T) {
	t.Parallel()
	if got := permalink(&responses.Items{Code: "DgKBiN1vLsH", ProductType: "clips"}); got != "https://www.instagram.com/reel/DgKBiN1vLsH/" {
		t.Errorf("reel permalink = %q", got)
	}
	if got := permalink(&responses.Items{Code: "Bxyz", ProductType: "feed"}); got != "https://www.instagram.com/p/Bxyz/" {
		t.Errorf("post permalink = %q", got)
	}
	if got := permalink(&responses.Items{}); got != "" {
		t.Errorf("permalink without code = %q", got)
	}
}

// forwardedCommentContent is a comment forwarded from a reel, as Instagram's
// web API sends it (CDN URLs shortened): the commenter, the comment and the
// reel's cover, with no target id or URL anywhere on the message.
const forwardedCommentContent = `{"__isSlideMessageContent":"SlideMessageXMAContent","__typename":"SlideMessageXMAContent",
	"xma":{"__typename":"SlideMessageLayeredXMA","collapsible_id":null,
	"favicon":{"fallback_url":"https://cdn/avatar.jpg","url":"https://cdn/avatar.jpg"},
	"preview_image":{"fallback_url":"","height":1280,"preview_image_decoration_type":"REEL","url":"https://cdn/cover.jpg","width":720},
	"subtitle_text":"Cart 🛒 nahi hoga tempo 🚚 me add Kiya hoga 😂","target_url":null,"title_text":"gujrat_lions.1",
	"xmaPreviewImage":{"fallback_url":"","preview_image_decoration_type":"REEL","url":"https://cdn/cover.jpg"},
	"xmaTitle":"gujrat_lions.1"},"xma_text_body":""}`

func TestHandleForwardedReelComment(t *testing.T) {
	t.Parallel()
	doer := &stubDoer{}
	fetcher := &postFetcher{response: post(videoItem("https://cdn/reel.mp4"))}
	broker := &stubBroker{}
	n, ch := newTestNormalizer(broker, doer, fetcher)
	handle(t, n, messageDelta(groupShortID, "mid.1", peerID, forwardedCommentContent))

	create := drain(ch)[0].GetMessageCreate()
	want := "gujrat_lions.1 commented on a reel:\nCart 🛒 nahi hoga tempo 🚚 me add Kiya hoga 😂\n" +
		"[forwarded comment: open Instagram to view the reel]"
	if create.GetText() != want {
		t.Fatalf("text = %q, want %q", create.GetText(), want)
	}
	if len(fetcher.asked) != 0 {
		t.Fatalf("FetchMedia asked %v, want none: the share names no reel", fetcher.asked)
	}
	if len(doer.requests) != 1 || doer.requests[0] != "https://cdn/cover.jpg" || broker.uploaded[0].GetKind() != wuphfpb.Attachment_KIND_IMAGE {
		t.Fatalf("requests = %v kind = %v, want the reel's cover", doer.requests, broker.uploaded[0].GetKind())
	}
}

// stubComments answers comment lookups with one canned result and records
// what was asked.
type stubComments struct {
	target commentTarget
	err    error
	asked  [][2]string
}

func (s *stubComments) ResolveComment(_ context.Context, threadLongID, messageID string) (commentTarget, error) {
	s.asked = append(s.asked, [2]string{threadLongID, messageID})
	return s.target, s.err
}

func newCommentNormalizer(broker MediaBroker, doer HTTPDoer, fetcher MediaFetcher, comments CommentResolver) (*Normalizer, chan *wuphfpb.ClientFrame) {
	ch := make(chan *wuphfpb.ClientFrame, 16)
	threads, _ := newTestDirectory()
	return NewNormalizer(ch, NewMediaCodec(broker, doer, fetcher), threads, comments, testOwnUserID, zerolog.Nop()), ch
}

func TestHandleForwardedReelCommentResolvesTheReel(t *testing.T) {
	t.Parallel()
	doer := &stubDoer{}
	fetcher := &postFetcher{response: post(videoItem("https://cdn/reel.mp4"))}
	broker := &stubBroker{}
	comments := &stubComments{target: commentTarget{
		link:    shareLink{kind: "reel", shortcode: "DgHVFxWsY-a"},
		mediaID: "4037288346529140634",
	}}
	n, ch := newCommentNormalizer(broker, doer, fetcher, comments)
	handle(t, n, messageDelta(groupShortID, "mid.1", peerID, forwardedCommentContent))

	create := drain(ch)[0].GetMessageCreate()
	want := "gujrat_lions.1 commented on a reel:\nCart 🛒 nahi hoga tempo 🚚 me add Kiya hoga 😂\n" +
		"https://www.instagram.com/reel/DgHVFxWsY-a/"
	if create.GetText() != want {
		t.Fatalf("text = %q, want %q", create.GetText(), want)
	}
	// The thread is named by its long id, as the app API addresses it.
	if len(comments.asked) != 1 || comments.asked[0] != [2]string{"long-42", "mid.1"} {
		t.Fatalf("resolver asked %v", comments.asked)
	}
	if len(fetcher.asked) != 1 || fetcher.asked[0] != [2]string{"4037288346529140634", "DgHVFxWsY-a"} {
		t.Fatalf("FetchMedia asked %v", fetcher.asked)
	}
	if len(doer.requests) != 1 || doer.requests[0] != "https://cdn/reel.mp4" || broker.uploaded[0].GetKind() != wuphfpb.Attachment_KIND_VIDEO {
		t.Fatalf("requests = %v kind = %v, want the reel video", doer.requests, broker.uploaded[0].GetKind())
	}
}

// A failed lookup leaves the comment, the note and the cover.
func TestHandleForwardedReelCommentLookupFailureKeepsNote(t *testing.T) {
	t.Parallel()
	doer := &stubDoer{}
	fetcher := &postFetcher{}
	broker := &stubBroker{}
	n, ch := newCommentNormalizer(broker, doer, fetcher, &stubComments{err: errCommentNotFound})
	handle(t, n, messageDelta(groupShortID, "mid.1", peerID, forwardedCommentContent))

	create := drain(ch)[0].GetMessageCreate()
	if !strings.HasSuffix(create.GetText(), "[forwarded comment: open Instagram to view the reel]") {
		t.Fatalf("text = %q", create.GetText())
	}
	if len(fetcher.asked) != 0 || len(doer.requests) != 1 || doer.requests[0] != "https://cdn/cover.jpg" {
		t.Fatalf("FetchMedia asked %v requests = %v, want only the cover", fetcher.asked, doer.requests)
	}
}

func TestForwardedCommentText(t *testing.T) {
	t.Parallel()
	body := xmaBody(decodeXMA(t, `{"__typename":"SlideMessageLayeredXMA","title_text":"pam","subtitle_text":"nice"}`))
	if body.text != "pam commented on a post:\nnice\n[forwarded comment: open Instagram to view the post]" {
		t.Errorf("post comment text = %q", body.text)
	}
	if body.unresolvedShare != nil {
		t.Errorf("a forwarded comment is a known shape, not logged as unresolved")
	}
	// A layered share that names its post bridges as a normal share.
	body = xmaBody(decodeXMA(t, `{"__typename":"SlideMessageLayeredXMA","title_text":"pam","subtitle_text":"nice",
		"target_id":"999","target_url":"https://www.instagram.com/reel/AbC_1/"}`))
	if body.text != "pam\nnice\nhttps://www.instagram.com/reel/AbC_1/" {
		t.Errorf("targeted layered share text = %q", body.text)
	}
}

func TestHandleShareWithOnlyTargetIDLearnsLinkFromFetch(t *testing.T) {
	t.Parallel()
	response := post(videoItem("https://cdn/reel.mp4"))
	response.Items[0].Code, response.Items[0].ProductType = "DgKBiN1vLsH", "clips"
	fetcher := &postFetcher{response: response}
	broker := &stubBroker{}
	n, ch := newTestNormalizer(broker, &stubDoer{}, fetcher)
	handle(t, n, messageDelta(groupShortID, "mid.1", peerID, `{"__typename":"SlideMessageXMAContent",
		"xma":{"target_id":"4038046000000000000","title_text":"funny","preview_image":{"url":"https://cdn/preview.jpg"}}}`))

	create := drain(ch)[0].GetMessageCreate()
	if create.GetText() != "funny\nhttps://www.instagram.com/reel/DgKBiN1vLsH/" {
		t.Fatalf("text = %q", create.GetText())
	}
	if len(fetcher.asked) != 1 || fetcher.asked[0] != [2]string{"4038046000000000000", ""} {
		t.Fatalf("FetchMedia asked %v", fetcher.asked)
	}
	if broker.uploaded[0].GetKind() != wuphfpb.Attachment_KIND_VIDEO {
		t.Fatalf("kind = %v, want video", broker.uploaded[0].GetKind())
	}
}

// A share that names no post or reel and is no known shape still bridges its
// text and preview, and is flagged for the diagnostic log.
func TestUnresolvableShareKeepsPreview(t *testing.T) {
	t.Parallel()
	fetcher := &postFetcher{response: post(videoItem("https://cdn/reel.mp4"))}
	doer := &stubDoer{}
	broker := &stubBroker{}
	n, ch := newTestNormalizer(broker, doer, fetcher)
	content := `{"__typename":"SlideMessageXMAContent","xma":{"__typename":"SlideMessageSomethingNewXMA",
		"title_text":"title","preview_image":{"url":"https://cdn/thumb.jpg"}}}`
	handle(t, n, messageDelta(groupShortID, "mid.1", peerID, content))

	create := drain(ch)[0].GetMessageCreate()
	if create.GetText() != "title" {
		t.Fatalf("text = %q", create.GetText())
	}
	if len(fetcher.asked) != 0 {
		t.Fatalf("FetchMedia asked %v, want none", fetcher.asked)
	}
	if len(doer.requests) != 1 || doer.requests[0] != "https://cdn/thumb.jpg" || broker.uploaded[0].GetKind() != wuphfpb.Attachment_KIND_IMAGE {
		t.Fatalf("requests = %v kind = %v, want the preview image", doer.requests, broker.uploaded[0].GetKind())
	}
	if body := xmaBody(decodeXMA(t, `{"__typename":"SlideMessageSomethingNewXMA","title_text":"title"}`)); body.unresolvedShare == nil {
		t.Fatalf("unresolved share not flagged for logging")
	}
}

// A post or reel shared by link alone (no target id) resolves to full quality
// from its shortcode; the link already in the text is not repeated.
func TestHandleReelShareWithoutTargetIDDecodesShortcode(t *testing.T) {
	t.Parallel()
	fetcher := &postFetcher{response: post(videoItem("https://cdn/reel.mp4"))}
	n, ch := newTestNormalizer(&stubBroker{}, &stubDoer{}, fetcher)
	handle(t, n, messageDelta(groupShortID, "mid.1", peerID, `{"__typename":"SlideMessageXMAContent",
		"xma":{"target_url":"https://www.instagram.com/reel/DgKBiN1vLsH/","title_text":"funny",
		"preview_image":{"url":"https://cdn/preview.jpg"}}}`))

	create := drain(ch)[0].GetMessageCreate()
	if create.GetText() != "funny\nhttps://www.instagram.com/reel/DgKBiN1vLsH/" {
		t.Fatalf("text = %q", create.GetText())
	}
	mediaID, _ := MediaIDFromShortcode("DgKBiN1vLsH")
	if len(fetcher.asked) != 1 || fetcher.asked[0] != [2]string{mediaID, "DgKBiN1vLsH"} {
		t.Fatalf("FetchMedia asked %v, want %s", fetcher.asked, mediaID)
	}
}
