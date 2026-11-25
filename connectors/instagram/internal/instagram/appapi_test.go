package instagramconnector

import (
	"context"
	"encoding/base64"
	"errors"
	"io"
	"net/http"
	"strings"
	"testing"

	"go.mau.fi/mautrix-meta/pkg/messagix/cookies"
	"go.mau.fi/mautrix-meta/pkg/messagix/types"
	"go.mau.fi/mautrix-meta/pkg/messagix/useragent"
)

// forwardedCommentItem is a forwarded comment as the app API returns it
// (trimmed to the fields that matter, CDN URLs dropped).
const forwardedCommentItem = `{"item_type":"generic_xma","message_id":"mid.comment","auxiliary_text":" sent a comment",
	"generic_xma":[{"title_text":"gujrat_lions.1","subtitle_text":"Cart nahi hoga","target_url":null,
	"preview_image_decoration_type":8,"preview_media_fbid":18210874027370410,
	"cta_buttons":[
		{"action_url":"instagram://comments?media_id=4037288346529140634_62919895894&comment_id=18210874027370410&permalink_enabled=1","cta_type":"xma_ig_comment_action_url","title":""},
		{"action_url":"https://www.instagram.com/reel/DgHVFxWsY-a/","cta_type":"xma_ig_comment_media_action_url","title":""}]}]}`

const otherItem = `{"item_type":"xma_clip","message_id":"mid.other"}`

func threadPageJSON(hasOlder bool, cursor string, items ...string) string {
	older := "false"
	if hasOlder {
		older = "true"
	}
	return `{"thread":{"items":[` + strings.Join(items, ",") + `],"has_older":` + older + `,"oldest_cursor":"` + cursor + `"},"status":"ok"}`
}

// pagedDoer answers each request with the next canned body and records the
// requests.
type pagedDoer struct {
	status   int
	bodies   []string
	requests []*http.Request
}

func (d *pagedDoer) Do(request *http.Request) (*http.Response, error) {
	d.requests = append(d.requests, request)
	status := d.status
	if status == 0 {
		status = http.StatusOK
	}
	body := `{"thread":{"items":[]}}`
	if len(d.bodies) > 0 {
		body, d.bodies = d.bodies[0], d.bodies[1:]
	}
	return &http.Response{StatusCode: status, Status: http.StatusText(status), Body: io.NopCloser(strings.NewReader(body))}, nil
}

func testJar() *cookies.Cookies {
	jar := &cookies.Cookies{Platform: types.Instagram}
	jar.UpdateValues(map[cookies.MetaCookieName]string{
		cookies.IGCookieSessionID: "sess",
		cookies.IGCookieDSUserID:  "555000",
		cookies.IGCookieMachineID: "mid-1",
	})
	return jar
}

func newTestAppAPI(doer HTTPDoer) *AppAPI {
	jar := testJar()
	return NewAppAPI(doer, func() *cookies.Cookies { return jar })
}

func TestAppAPIResolvesForwardedComment(t *testing.T) {
	t.Parallel()
	doer := &pagedDoer{bodies: []string{threadPageJSON(true, "c1", otherItem, forwardedCommentItem)}}
	target, err := newTestAppAPI(doer).ResolveComment(context.Background(), "3402823668417", "mid.comment")
	if err != nil {
		t.Fatal(err)
	}
	if target.link.canonical() != "https://www.instagram.com/reel/DgHVFxWsY-a/" || target.mediaID != "4037288346529140634" {
		t.Fatalf("target = %+v", target)
	}
	if len(doer.requests) != 1 {
		t.Fatalf("requests = %d, want 1", len(doer.requests))
	}
	request := doer.requests[0]
	if request.URL.Host != "i.instagram.com" || request.URL.Path != "/api/v1/direct_v2/threads/3402823668417/" ||
		request.URL.Query().Get("limit") != "20" || request.URL.Query().Has("cursor") {
		t.Fatalf("url = %s", request.URL)
	}
	token, _ := base64.StdEncoding.DecodeString(strings.TrimPrefix(request.Header.Get("Authorization"), "Bearer IGT:2:"))
	if string(token) != `{"ds_user_id":"555000","sessionid":"sess"}` {
		t.Fatalf("authorization = %q (token %s)", request.Header.Get("Authorization"), token)
	}
	if request.Header.Get("User-Agent") != useragent.AndroidUserAgent || request.Header.Get("X-IG-App-ID") != useragent.IGAndroidAppID ||
		request.Header.Get("IG-Intended-User-ID") != "555000" || request.Header.Get("X-MID") != "mid-1" {
		t.Fatalf("headers = %v", request.Header)
	}
}

func TestAppAPIPagesBackToTheComment(t *testing.T) {
	t.Parallel()
	doer := &pagedDoer{bodies: []string{
		threadPageJSON(true, "c1", otherItem),
		threadPageJSON(true, "c2", forwardedCommentItem),
	}}
	if _, err := newTestAppAPI(doer).ResolveComment(context.Background(), "t", "mid.comment"); err != nil {
		t.Fatal(err)
	}
	if len(doer.requests) != 2 || doer.requests[1].URL.Query().Get("cursor") != "c1" {
		t.Fatalf("requests = %v", doer.requests)
	}
}

func TestAppAPIStopsLooking(t *testing.T) {
	t.Parallel()
	// The thread has no older page.
	doer := &pagedDoer{bodies: []string{threadPageJSON(false, "", otherItem)}}
	if _, err := newTestAppAPI(doer).ResolveComment(context.Background(), "t", "mid.comment"); !errors.Is(err, errCommentNotFound) || len(doer.requests) != 1 {
		t.Fatalf("err = %v requests = %d", err, len(doer.requests))
	}
	// Older pages exist but the bound is reached.
	pages := make([]string, appThreadMaxPages+1)
	for i := range pages {
		pages[i] = threadPageJSON(true, "c", otherItem)
	}
	doer = &pagedDoer{bodies: pages}
	if _, err := newTestAppAPI(doer).ResolveComment(context.Background(), "t", "mid.comment"); !errors.Is(err, errCommentNotFound) || len(doer.requests) != appThreadMaxPages {
		t.Fatalf("err = %v requests = %d", err, len(doer.requests))
	}
}

func TestAppAPIFailures(t *testing.T) {
	t.Parallel()
	if _, err := newTestAppAPI(&pagedDoer{status: http.StatusForbidden}).ResolveComment(context.Background(), "t", "m"); err == nil {
		t.Error("a non-200 page must fail")
	}
	noSession := NewAppAPI(&pagedDoer{}, func() *cookies.Cookies { return &cookies.Cookies{Platform: types.Instagram} })
	if _, err := noSession.ResolveComment(context.Background(), "t", "m"); err == nil {
		t.Error("a missing session must fail")
	}
	// The comment's item without its post's link names nothing.
	bare := `{"item_type":"generic_xma","message_id":"mid.comment","generic_xma":[{"cta_buttons":[]}]}`
	doer := &pagedDoer{bodies: []string{threadPageJSON(false, "", bare)}}
	if _, err := newTestAppAPI(doer).ResolveComment(context.Background(), "t", "mid.comment"); !errors.Is(err, errCommentNotFound) {
		t.Errorf("err = %v, want not found", err)
	}
}

func TestCommentTargetDecodesShortcodeWithoutDeepLink(t *testing.T) {
	t.Parallel()
	item := `{"item_type":"generic_xma","message_id":"mid.comment","generic_xma":[{"cta_buttons":[
		{"action_url":"https://www.instagram.com/reel/DgHVFxWsY-a/","cta_type":"xma_ig_comment_media_action_url"}]}]}`
	doer := &pagedDoer{bodies: []string{threadPageJSON(false, "", item)}}
	target, err := newTestAppAPI(doer).ResolveComment(context.Background(), "t", "mid.comment")
	if err != nil {
		t.Fatal(err)
	}
	// The shortcode decodes to the id the deep link names.
	if target.mediaID != "4037288346529140634" {
		t.Fatalf("media id = %s", target.mediaID)
	}
}

func TestDeepLinkMediaID(t *testing.T) {
	t.Parallel()
	for raw, want := range map[string]string{
		"instagram://comments?media_id=4037288346529140634_62919895894&comment_id=1": "4037288346529140634",
		"instagram://comments?media_id=4037288346529140634":                          "4037288346529140634",
		"instagram://comments?comment_id=1":                                          "",
		"instagram://comments?media_id=abc_1":                                        "",
		"%zz":                                                                        "",
	} {
		if got := deepLinkMediaID(raw); got != want {
			t.Errorf("deepLinkMediaID(%q) = %q, want %q", raw, got, want)
		}
	}
}
