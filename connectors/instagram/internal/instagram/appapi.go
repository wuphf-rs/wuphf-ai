package instagramconnector

import (
	"context"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"strconv"
	"strings"

	"go.mau.fi/mautrix-meta/pkg/messagix/cookies"
	"go.mau.fi/mautrix-meta/pkg/messagix/httpclient"
	"go.mau.fi/mautrix-meta/pkg/messagix/useragent"
)

// appAPIBaseURL is the host Instagram's Android app talks REST to.
const appAPIBaseURL = "https://i.instagram.com"

// appThreadPageSize and appThreadMaxPages bound how far back a forwarded
// comment is looked for: it is looked up as it arrives, so it is almost always
// on the first page.
const (
	appThreadPageSize = 20
	appThreadMaxPages = 3
)

// maxAppResponseBytes caps one thread page; a page of 20 items is ~200 KiB.
const maxAppResponseBytes = 8 * 1024 * 1024

// CTA types on a forwarded comment's app item: the comment (an instagram://
// deep link naming the media id) and the post it was left on (its public
// link).
const (
	commentActionCTA      = "xma_ig_comment_action_url"
	commentMediaActionCTA = "xma_ig_comment_media_action_url"
)

// errCommentNotFound is returned when the message is not among the pages
// looked at, or its item names no post.
var errCommentNotFound = errors.New("forwarded comment's post not found in the app thread")

// commentTarget is the post a forwarded comment was left on.
type commentTarget struct {
	link    shareLink
	mediaID string
}

// CommentResolver finds the post a forwarded comment was left on. The web API
// the connector runs on omits it; Instagram's app API carries it.
type CommentResolver interface {
	ResolveComment(ctx context.Context, threadLongID, messageID string) (commentTarget, error)
}

// AppAPI reads threads from Instagram's Android app REST API, authenticated
// with the same session the web client uses (the app's bearer token is built
// from the session cookies). It is used only to resolve forwarded comments.
type AppAPI struct {
	http    HTTPDoer
	cookies func() *cookies.Cookies
	baseURL string
}

// NewAppAPI reads the session from jar on every request, so refreshed
// cookies are picked up.
func NewAppAPI(doer HTTPDoer, jar func() *cookies.Cookies) *AppAPI {
	return &AppAPI{http: doer, cookies: jar, baseURL: appAPIBaseURL}
}

// appThreadPage is the part of a direct_v2 thread page the resolver reads.
type appThreadPage struct {
	Thread struct {
		Items        []appThreadItem `json:"items"`
		OldestCursor string          `json:"oldest_cursor"`
		HasOlder     bool            `json:"has_older"`
	} `json:"thread"`
}

type appThreadItem struct {
	MessageID  string `json:"message_id"`
	GenericXMA []struct {
		CTAButtons []struct {
			ActionURL string `json:"action_url"`
			CTAType   string `json:"cta_type"`
		} `json:"cta_buttons"`
	} `json:"generic_xma"`
}

// ResolveComment pages back through the thread until it finds the message
// and reads the post off its CTA buttons.
func (a *AppAPI) ResolveComment(ctx context.Context, threadLongID, messageID string) (commentTarget, error) {
	cursor := ""
	for range appThreadMaxPages {
		page, err := a.threadPage(ctx, threadLongID, cursor)
		if err != nil {
			return commentTarget{}, err
		}
		for _, item := range page.Thread.Items {
			if item.MessageID == messageID {
				return item.commentTarget()
			}
		}
		if !page.Thread.HasOlder || page.Thread.OldestCursor == "" {
			break
		}
		cursor = page.Thread.OldestCursor
	}
	return commentTarget{}, errCommentNotFound
}

// commentTarget reads the post's link from the media CTA and its media id
// from the comment's deep link, decoding the shortcode when the deep link is
// missing.
func (item appThreadItem) commentTarget() (commentTarget, error) {
	var target commentTarget
	found := false
	for _, xma := range item.GenericXMA {
		for _, button := range xma.CTAButtons {
			switch button.CTAType {
			case commentMediaActionCTA:
				target.link, found = parseShareLink(button.ActionURL)
			case commentActionCTA:
				target.mediaID = deepLinkMediaID(button.ActionURL)
			}
		}
	}
	if !found {
		return commentTarget{}, errCommentNotFound
	}
	if target.mediaID == "" {
		mediaID, err := MediaIDFromShortcode(target.link.shortcode)
		if err != nil {
			return commentTarget{}, err
		}
		target.mediaID = mediaID
	}
	return target, nil
}

// deepLinkMediaID reads the media id out of an instagram://comments?media_id=
// <media>_<owner> link, or "" when there is none.
func deepLinkMediaID(raw string) string {
	link, err := url.Parse(raw)
	if err != nil {
		return ""
	}
	mediaID, _, _ := strings.Cut(link.Query().Get("media_id"), "_")
	if !isNumericID(mediaID) {
		return ""
	}
	return mediaID
}

func (a *AppAPI) threadPage(ctx context.Context, threadLongID, cursor string) (*appThreadPage, error) {
	query := url.Values{"limit": {strconv.Itoa(appThreadPageSize)}}
	if cursor != "" {
		query.Set("cursor", cursor)
	}
	endpoint := fmt.Sprintf("%s/api/v1/direct_v2/threads/%s/?%s", a.baseURL, url.PathEscape(threadLongID), query.Encode())
	request, err := http.NewRequestWithContext(ctx, http.MethodGet, endpoint, nil)
	if err != nil {
		return nil, fmt.Errorf("build Instagram app thread request: %w", err)
	}
	headers, err := a.headers()
	if err != nil {
		return nil, err
	}
	request.Header = headers
	response, err := a.http.Do(request)
	if err != nil {
		return nil, fmt.Errorf("fetch Instagram app thread: %w", err)
	}
	defer response.Body.Close()
	if response.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("fetch Instagram app thread: unexpected status %s", response.Status)
	}
	var page appThreadPage
	if err := json.NewDecoder(io.LimitReader(response.Body, maxAppResponseBytes)).Decode(&page); err != nil {
		return nil, fmt.Errorf("decode Instagram app thread: %w", err)
	}
	return &page, nil
}

// headers are the Android app's, with the bearer token the app derives from
// the session cookies (the form instameow itself sends to i.instagram.com).
func (a *AppAPI) headers() (http.Header, error) {
	jar := a.cookies()
	if jar == nil || jar.Get(cookies.IGCookieSessionID) == "" {
		return nil, errors.New("no Instagram session for the app API")
	}
	token, err := json.Marshal(httpclient.RUploadToken{
		DSUserID:  jar.Get(cookies.IGCookieDSUserID),
		SessionID: jar.Get(cookies.IGCookieSessionID),
	})
	if err != nil {
		return nil, err
	}
	userID := strconv.FormatInt(jar.GetUserID(), 10)
	headers := http.Header{}
	headers.Set("Authorization", "Bearer IGT:2:"+base64.StdEncoding.EncodeToString(token))
	headers.Set("User-Agent", useragent.AndroidUserAgent)
	headers.Set("IG-Intended-User-ID", userID)
	headers.Set("IG-U-DS-User-ID", userID)
	headers.Set("X-MID", jar.Get(cookies.IGCookieMachineID))
	headers.Set("X-IG-App-ID", useragent.IGAndroidAppID)
	return headers, nil
}
