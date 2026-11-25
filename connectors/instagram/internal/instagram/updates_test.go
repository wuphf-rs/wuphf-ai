package instagramconnector

import (
	"context"
	"encoding/json"
	"errors"
	"io"
	"net/http"
	"strconv"
	"strings"
	"testing"

	"github.com/rs/zerolog"
	wuphfpb "github.com/wuphf-rs/wuphf-ai/proto/go/connector/v1"
	"go.mau.fi/mautrix-meta/pkg/instameow"
	"go.mau.fi/mautrix-meta/pkg/instameow/slidetypes"
	"go.mau.fi/mautrix-meta/pkg/messagix/responses"
)

type stubBroker struct{ uploaded []*wuphfpb.MediaMetadata }

func (s *stubBroker) Upload(_ context.Context, meta *wuphfpb.MediaMetadata, _ []byte) (*wuphfpb.Attachment, error) {
	s.uploaded = append(s.uploaded, meta)
	return &wuphfpb.Attachment{
		MediaId:  "media-" + strconv.Itoa(len(s.uploaded)),
		Kind:     meta.GetKind(),
		MimeType: meta.MimeType,
		FileName: meta.FileName,
	}, nil
}

type stubDoer struct {
	status   int
	requests []string
}

func (s *stubDoer) Do(req *http.Request) (*http.Response, error) {
	s.requests = append(s.requests, req.URL.String())
	status := s.status
	if status == 0 {
		status = http.StatusOK
	}
	return &http.Response{
		StatusCode: status,
		Status:     http.StatusText(status),
		Header:     http.Header{"Content-Type": {"image/jpeg; charset=binary"}},
		Body:       io.NopCloser(strings.NewReader("bytes")),
	}, nil
}

type stubFetcher struct {
	resp *responses.FetchMediaResponse
	err  error
}

func (s stubFetcher) FetchMedia(context.Context, string, string) (*responses.FetchMediaResponse, error) {
	return s.resp, s.err
}

// stubThreads answers thread lookups from a fixed set of thread objects and
// counts the round trips a cache miss costs.
type stubThreads struct {
	byShort    map[string]*slidetypes.ThreadInfo
	ids        map[int64]*instameow.ThreadIGIDs
	getThreads int
	fetchIDs   int
}

func (s *stubThreads) GetThread(_ context.Context, req *slidetypes.GetThreadInfoRequest) (*slidetypes.ThreadInfoResponse, error) {
	s.getThreads++
	info, ok := s.byShort[req.ThreadFBID]
	if !ok {
		return nil, errors.New("not found")
	}
	return &slidetypes.ThreadInfoResponse{ThreadInfo: slidetypes.WrappedThreadInfo{AsIGDirectThread: info}}, nil
}

func (s *stubThreads) FetchThreadID(_ context.Context, threadKey int64) (*instameow.ThreadIGIDs, error) {
	s.fetchIDs++
	ids, ok := s.ids[threadKey]
	if !ok {
		return nil, instameow.ErrThreadNotFound
	}
	return ids, nil
}

// testOwnUserID is the logged-in account id used by test normalizers; senders
// with this id are labelled "Me".
const testOwnUserID int64 = 555000

// Thread fixtures: a group known from the inbox listing and a one-to-one
// thread only reachable through a lookup.
const (
	groupShortID       = "340282366841710300949128100000000001"
	groupKey     int64 = 42
	dmShortID          = "340282366841710300949128100000000002"
	dmKey        int64 = 77
	peerID       int64 = 7
)

func newTestDirectory() (*ThreadDirectory, *stubThreads) {
	fetcher := &stubThreads{
		byShort: map[string]*slidetypes.ThreadInfo{
			dmShortID: {
				ThreadKey: dmKey,
				Users:     []*slidetypes.User{{InteropMessagingUserFBID: peerID, FullName: "Pam"}},
			},
		},
		ids: map[int64]*instameow.ThreadIGIDs{},
	}
	threads := NewThreadDirectory(fetcher, testOwnUserID)
	threads.RecordThread(&slidetypes.ThreadInfo{
		ID: groupShortID, ThreadKey: groupKey, ThreadID: "long-42",
		IsGroup: true, ThreadTitle: "Squad",
	})
	return threads, fetcher
}

func newTestNormalizer(broker MediaBroker, doer HTTPDoer, fetcher MediaFetcher) (*Normalizer, chan *wuphfpb.ClientFrame) {
	ch := make(chan *wuphfpb.ClientFrame, 16)
	threads, _ := newTestDirectory()
	return NewNormalizer(ch, NewMediaCodec(broker, doer, fetcher), threads, nil, testOwnUserID, zerolog.Nop()), ch
}

func decodeDelta(t *testing.T, raw string) *slidetypes.Delta {
	t.Helper()
	var delta slidetypes.Delta
	if err := json.Unmarshal([]byte(raw), &delta); err != nil {
		t.Fatalf("decode delta: %v", err)
	}
	return &delta
}

// messageDelta builds a new-message delta in the shape Instagram sends.
func messageDelta(thread, id string, sender int64, content string) string {
	return `{"__typename":"SlideUQPPNewMessage","uq_seq_id":"10","thread_fbid":"` + thread + `",
		"message":{"id":"` + id + `","sender_fbid":"` + strconv.FormatInt(sender, 10) + `",
		"timestamp_ms":"1700000000000","content":` + content + `}}`
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

func handle(t *testing.T, n *Normalizer, raw string) {
	t.Helper()
	if err := n.Handle(context.Background(), decodeDelta(t, raw)); err != nil {
		t.Fatalf("Handle() error = %v", err)
	}
}

func TestHandleCreateCarriesGroupTitleAndReply(t *testing.T) {
	t.Parallel()
	n, ch := newTestNormalizer(&stubBroker{}, &stubDoer{}, nil)
	handle(t, n, `{"__typename":"SlideUQPPNewMessage","thread_fbid":"`+groupShortID+`",
		"message":{"id":"mid.1","sender_fbid":"7","timestamp_ms":"1700000000000",
		"replied_to_message_id":"mid.0",
		"sender":{"name":"Pam","user_dict":{"full_name":"Pam Beesly","interop_messaging_user_fbid":"7"}},
		"content":{"__typename":"SlideMessageText","text_body":"hello"}}}`)

	frames := drain(ch)
	if len(frames) != 1 {
		t.Fatalf("frames = %d, want 1", len(frames))
	}
	create := frames[0].GetMessageCreate()
	if create.GetChatId() != "42" || create.GetChatTitle() != "[G] Squad" {
		t.Fatalf("chat = %q title = %q", create.GetChatId(), create.GetChatTitle())
	}
	if create.GetPlatformMessageId() != "mid.1" || create.GetText() != "hello" || create.GetTimestampMs() != 1700000000000 {
		t.Fatalf("create = %+v", create)
	}
	if create.GetSender() != "7" || create.GetParticipant() != "7" || create.GetSenderName() != "Pam Beesly" {
		t.Fatalf("sender = %q participant = %q name = %q", create.GetSender(), create.GetParticipant(), create.GetSenderName())
	}
	if create.GetReplyToPlatformMessageId() != "mid.0" {
		t.Fatalf("reply = %q", create.GetReplyToPlatformMessageId())
	}
}

// A thread first seen in a live delta is looked up once, named after the peer
// for a one-to-one chat, and served from the cache afterwards.
func TestHandleCreateResolvesUnknownThreadOnce(t *testing.T) {
	t.Parallel()
	threads, fetcher := newTestDirectory()
	ch := make(chan *wuphfpb.ClientFrame, 16)
	n := NewNormalizer(ch, NewMediaCodec(&stubBroker{}, &stubDoer{}, nil), threads, nil, testOwnUserID, zerolog.Nop())
	text := `{"__typename":"SlideMessageText","text_body":"hi"}`
	handle(t, n, messageDelta(dmShortID, "mid.1", peerID, text))
	handle(t, n, messageDelta(dmShortID, "mid.2", peerID, text))

	frames := drain(ch)
	if len(frames) != 2 || fetcher.getThreads != 1 {
		t.Fatalf("frames = %d, lookups = %d; want 2 frames from 1 lookup", len(frames), fetcher.getThreads)
	}
	create := frames[0].GetMessageCreate()
	if create.GetChatId() != "77" || create.GetChatTitle() != "[P] Pam" || create.GetSenderName() != "Pam" {
		t.Fatalf("chat = %q title = %q name = %q", create.GetChatId(), create.GetChatTitle(), create.GetSenderName())
	}
}

func TestHandleSkipsDeltaForUnresolvableThread(t *testing.T) {
	t.Parallel()
	n, ch := newTestNormalizer(&stubBroker{}, &stubDoer{}, nil)
	handle(t, n, messageDelta("unknown", "mid.1", peerID, `{"__typename":"SlideMessageText","text_body":"hi"}`))
	if frames := drain(ch); len(frames) != 0 {
		t.Fatalf("frames = %d, want 0", len(frames))
	}
}

func TestHandleOwnMessageLabelledMe(t *testing.T) {
	t.Parallel()
	n, ch := newTestNormalizer(&stubBroker{}, &stubDoer{}, nil)
	handle(t, n, messageDelta(groupShortID, "mid.1", testOwnUserID, `{"__typename":"SlideMessageText","text_body":"mine"}`))
	if name := drain(ch)[0].GetMessageCreate().GetSenderName(); name != "Me" {
		t.Fatalf("sender name = %q, want Me", name)
	}
}

// A message whose thread id is only on the message itself still resolves.
func TestHandleCreateFallsBackToMessageThreadID(t *testing.T) {
	t.Parallel()
	n, ch := newTestNormalizer(&stubBroker{}, &stubDoer{}, nil)
	handle(t, n, `{"__typename":"SlideUQPPNewRavenMessage","thread_fbid":"",
		"message":{"id":"mid.1","sender_fbid":"7","thread_fbid":"`+groupShortID+`","timestamp_ms":"1",
		"content":{"__typename":"SlideMessageRavenImageContent","view_mode":0}}}`)
	create := drain(ch)[0].GetMessageCreate()
	if create.GetChatId() != "42" || create.GetText() != viewOnceNotice {
		t.Fatalf("chat = %q text = %q", create.GetChatId(), create.GetText())
	}
}

func TestHandleImageDownloadsAndUploads(t *testing.T) {
	t.Parallel()
	broker, doer := &stubBroker{}, &stubDoer{}
	n, ch := newTestNormalizer(broker, doer, nil)
	handle(t, n, messageDelta(groupShortID, "mid.1", peerID, `{"__typename":"SlideMessageImageContent",
		"attachments":[{"attachment_cdn_url":"https://cdn/a.jpg"},{"preview_cdn_url":"https://cdn/b.jpg"}]}`))

	create := drain(ch)[0].GetMessageCreate()
	if len(create.GetAttachments()) != 2 || create.Text != nil {
		t.Fatalf("attachments = %d text = %v", len(create.GetAttachments()), create.Text)
	}
	if strings.Join(doer.requests, ",") != "https://cdn/a.jpg,https://cdn/b.jpg" {
		t.Fatalf("requests = %v", doer.requests)
	}
	if broker.uploaded[0].GetKind() != wuphfpb.Attachment_KIND_IMAGE || broker.uploaded[0].GetMimeType() != "image/jpeg" {
		t.Fatalf("metadata = %+v", broker.uploaded[0])
	}
}

// A failed download must not lose the message: the text still bridges.
func TestHandleMediaFailureKeepsText(t *testing.T) {
	t.Parallel()
	n, ch := newTestNormalizer(&stubBroker{}, &stubDoer{status: http.StatusForbidden}, nil)
	handle(t, n, messageDelta(groupShortID, "mid.1", peerID, `{"__typename":"SlideMessageXMAContent",
		"xma_text_body":"look",
		"xma":{"target_url":"https://example.com","preview_image":{"url":"https://cdn/p.jpg"}}}`))
	create := drain(ch)[0].GetMessageCreate()
	if create.GetText() != "look\nhttps://example.com" || len(create.GetAttachments()) != 0 {
		t.Fatalf("text = %q attachments = %d", create.GetText(), len(create.GetAttachments()))
	}
}

func TestHandleReelShareFetchesFullQualityVideo(t *testing.T) {
	t.Parallel()
	doer := &stubDoer{}
	fetcher := stubFetcher{resp: &responses.FetchMediaResponse{Items: []*responses.Items{{
		VideoVersions: []responses.VideoVersions{
			{URL: "https://cdn/small.mp4", Width: 320, Height: 568},
			{URL: "https://cdn/large.mp4", Width: 720, Height: 1280},
		},
	}}}}
	broker := &stubBroker{}
	n, ch := newTestNormalizer(broker, doer, fetcher)
	handle(t, n, messageDelta(groupShortID, "mid.1", peerID, `{"__typename":"SlideMessageXMAContent",
		"xma":{"target_id":"999","target_url":"https://www.instagram.com/reel/AbC_1/","title_text":"funny",
		"preview_image":{"url":"https://cdn/preview.jpg"}}}`))

	create := drain(ch)[0].GetMessageCreate()
	if create.GetText() != "funny\nhttps://www.instagram.com/reel/AbC_1/" {
		t.Fatalf("text = %q", create.GetText())
	}
	if len(doer.requests) != 1 || doer.requests[0] != "https://cdn/large.mp4" {
		t.Fatalf("requests = %v, want the largest rendition", doer.requests)
	}
	if broker.uploaded[0].GetKind() != wuphfpb.Attachment_KIND_VIDEO {
		t.Fatalf("kind = %v, want video", broker.uploaded[0].GetKind())
	}
}

func TestHandleReactionAdminLineDropped(t *testing.T) {
	t.Parallel()
	n, ch := newTestNormalizer(&stubBroker{}, &stubDoer{}, nil)
	handle(t, n, `{"__typename":"SlideUQPPAdminTextMessage","thread_fbid":"`+groupShortID+`",
		"message":{"id":"mid.1","sender_fbid":"7","timestamp_ms":"1",
		"content":{"__typename":"SlideMessageAdminText","is_reaction_action_log":true,
		"text_fragments":[{"plaintext":"Pam reacted"}]}}}`)
	if frames := drain(ch); len(frames) != 0 {
		t.Fatalf("frames = %d, want 0", len(frames))
	}
}

func TestHandleEditAndDelete(t *testing.T) {
	t.Parallel()
	n, ch := newTestNormalizer(&stubBroker{}, &stubDoer{}, nil)
	handle(t, n, `{"__typename":"SlideUQPPEditMessage","thread_fbid":"`+groupShortID+`","message_id":"mid.1","text_body":"fixed"}`)
	handle(t, n, `{"__typename":"SlideUQPPDeleteMessage","thread_fbid":"`+groupShortID+`","message_id":"mid.2"}`)

	frames := drain(ch)
	if len(frames) != 2 {
		t.Fatalf("frames = %d, want 2", len(frames))
	}
	edit := frames[0].GetMessageEdit()
	if edit.GetChatId() != "42" || edit.GetPlatformMessageId() != "mid.1" || edit.GetText() != "fixed" {
		t.Fatalf("edit = %+v", edit)
	}
	del := frames[1].GetMessageDelete()
	if del.GetChatId() != "42" || del.GetPlatformMessageId() != "mid.2" {
		t.Fatalf("delete = %+v", del)
	}
}

// A reaction removal that names only the reaction's log id is matched to the
// reaction seen earlier, so the right reactor's emoji is cleared.
func TestHandleReactionCreateAndRemoval(t *testing.T) {
	t.Parallel()
	n, ch := newTestNormalizer(&stubBroker{}, &stubDoer{}, nil)
	handle(t, n, `{"__typename":"SlideUQPPCreateReaction","thread_fbid":"`+groupShortID+`","message_id":"mid.1",
		"reaction":{"log_message_id":"log.1","reaction":"❤","sender_fbid":"555000"}}`)
	handle(t, n, `{"__typename":"SlideUQPPDeleteReaction","thread_fbid":"`+groupShortID+`","message_id":"log.1",
		"reaction":{"log_message_id":"log.1"}}`)
	handle(t, n, `{"__typename":"SlideUQPPDeleteReaction","thread_fbid":"`+groupShortID+`","message_id":"x",
		"reaction":{"log_message_id":"log.unknown"}}`)

	frames := drain(ch)
	if len(frames) != 2 {
		t.Fatalf("frames = %d, want 2 (unknown removal skipped)", len(frames))
	}
	added, removed := frames[0].GetReaction(), frames[1].GetReaction()
	if added.GetEmoji() != "❤" || !added.GetFromMe() || added.GetSenderName() != "Me" || added.GetPlatformMessageId() != "mid.1" {
		t.Fatalf("added = %+v", added)
	}
	if removed.GetEmoji() != "" || removed.GetPlatformMessageId() != "mid.1" || removed.GetSender() != "555000" {
		t.Fatalf("removed = %+v", removed)
	}
}

func TestHandleRenameRetitlesLaterMessages(t *testing.T) {
	t.Parallel()
	n, ch := newTestNormalizer(&stubBroker{}, &stubDoer{}, nil)
	handle(t, n, `{"__typename":"SlideUQPPThreadName","thread_fbid":"`+groupShortID+`","thread_name":"Office"}`)
	handle(t, n, messageDelta(groupShortID, "mid.1", peerID, `{"__typename":"SlideMessageText","text_body":"hi"}`))
	if title := drain(ch)[0].GetMessageCreate().GetChatTitle(); title != "[G] Office" {
		t.Fatalf("title = %q", title)
	}
}

func TestHandleIgnoresUnbridgedDeltas(t *testing.T) {
	t.Parallel()
	n, ch := newTestNormalizer(&stubBroker{}, &stubDoer{}, nil)
	handle(t, n, `{"__typename":"SlideUQPPMarkRead","thread_fbid":"`+groupShortID+`","read_timestamp_ms":"1"}`)
	handle(t, n, `{"__typename":"SlideUQPPSomethingNew","thread_fbid":"`+groupShortID+`"}`)
	if frames := drain(ch); len(frames) != 0 {
		t.Fatalf("frames = %d, want 0", len(frames))
	}
}

// A canceled context is the one failure surfaced, so instameow redelivers the
// delta after reconnecting instead of acknowledging a message never queued.
func TestHandleReturnsContextErrorWhenInputBlocked(t *testing.T) {
	t.Parallel()
	threads, _ := newTestDirectory()
	n := NewNormalizer(make(chan *wuphfpb.ClientFrame), NewMediaCodec(&stubBroker{}, &stubDoer{}, nil), threads, nil, testOwnUserID, zerolog.Nop())
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	delta := decodeDelta(t, messageDelta(groupShortID, "mid.1", peerID, `{"__typename":"SlideMessageText","text_body":"hi"}`))
	if err := n.Handle(ctx, delta); !errors.Is(err, context.Canceled) {
		t.Fatalf("err = %v, want context.Canceled", err)
	}
}
