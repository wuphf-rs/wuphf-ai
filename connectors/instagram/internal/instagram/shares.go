package instagramconnector

import (
	"fmt"
	"net/url"
	"regexp"
	"strings"

	"go.mau.fi/mautrix-meta/pkg/instameow/slidetypes"
	"go.mau.fi/mautrix-meta/pkg/messagix/responses"
)

// shareLink is the post or reel a share's target URL points at.
type shareLink struct {
	// kind is the link's path segment: "p", "reel" or "tv".
	kind      string
	shortcode string
}

// canonical is the public link of the post or reel, without the query.
func (l shareLink) canonical() string {
	return fmt.Sprintf("https://www.instagram.com/%s/%s/", l.kind, l.shortcode)
}

// shareLinkPath pulls the kind and shortcode out of a post or reel path:
// /p/<code>, /reel/<code>, /reels/<code>, /tv/<code>, each optionally under a
// /<user> prefix and followed by more path.
var shareLinkPath = regexp.MustCompile(`^(?:/[\w.]+)?/(p|reels?|tv)/([\w-]+)`)

// parseShareLink reads a post or reel link. Stories, profiles and other hosts
// are not share links.
func parseShareLink(raw string) (shareLink, bool) {
	link, err := url.Parse(strings.TrimSpace(raw))
	if err != nil || (link.Scheme != "http" && link.Scheme != "https") {
		return shareLink{}, false
	}
	switch strings.ToLower(link.Hostname()) {
	case "instagram.com", "www.instagram.com", "m.instagram.com":
	default:
		return shareLink{}, false
	}
	match := shareLinkPath.FindStringSubmatch(link.Path)
	if match == nil {
		return shareLink{}, false
	}
	kind := match[1]
	if kind == "reels" {
		kind = "reel"
	}
	return shareLink{kind: kind, shortcode: match[2]}, true
}

// shareRef identifies the media a full-quality fetch needs:
//
//   - a post or reel target link: the target id is the media id, else the id
//     is decoded from the shortcode;
//   - no target link but a numeric target id: the media is fetched by that id
//     and its link, learned from the fetch, is added to the text.
//
// Anything else (stories, plain links, forwarded comments) keeps the preview
// as its media.
func shareRef(xma *slidetypes.XMAContent) *shareMedia {
	if link, ok := parseShareLink(xma.TargetURL); ok {
		if isNumericID(xma.TargetID) {
			return &shareMedia{mediaID: xma.TargetID, shortcode: link.shortcode}
		}
		mediaID, err := MediaIDFromShortcode(link.shortcode)
		if err != nil {
			return nil
		}
		return &shareMedia{mediaID: mediaID, shortcode: link.shortcode}
	}
	if xma.TargetURL == "" && isNumericID(xma.TargetID) {
		return &shareMedia{mediaID: xma.TargetID, wantsLink: true}
	}
	return nil
}

// layeredXMATypename is the share Instagram sends for a comment forwarded from
// a post or reel: the commenter as title, the comment as subtitle, and the
// post's cover as preview. The web API names neither the post nor its link;
// the app API does, so the normalizer looks the post up there.
const layeredXMATypename = "SlideMessageLayeredXMA"

// forwardedCommentNotice ends a forwarded comment's text when its post could
// not be looked up.
const forwardedCommentNotice = "[forwarded comment: open Instagram to view the %s]"

// forwardedComment is a comment forwarded from a post or reel.
type forwardedComment struct {
	// prefix is the text the sender typed with the share, if any.
	prefix    string
	commenter string
	comment   string
	// post is "reel" or "post", from the cover's decoration.
	post string
}

// parseForwardedComment reports whether a share is a forwarded comment. A
// layered share that names its post bridges as a normal share instead.
func parseForwardedComment(content *slidetypes.MessageContentXMA) (*forwardedComment, bool) {
	xma := content.XMA
	if xma == nil || xma.Typename != layeredXMATypename || xma.TargetURL != "" || xma.TargetID != "" {
		return nil, false
	}
	commenter := xma.TitleText
	if commenter == "" {
		commenter = xma.XMATitle
	}
	if commenter == "" || xma.SubtitleText == "" {
		return nil, false
	}
	post := "post"
	if previewDecoration(xma) == "REEL" {
		post = "reel"
	}
	return &forwardedComment{prefix: content.XMATextBody, commenter: commenter, comment: xma.SubtitleText, post: post}, true
}

// text renders the comment, ending with the post's link, or with a note that
// the post is only on Instagram when link is "".
func (c *forwardedComment) text(link string) string {
	text := appendLine(c.prefix, c.commenter+" commented on a "+c.post+":\n"+c.comment)
	if link == "" {
		link = fmt.Sprintf(forwardedCommentNotice, c.post)
	}
	return appendLine(text, link)
}

func previewDecoration(xma *slidetypes.XMAContent) string {
	for _, preview := range []*slidetypes.XMAPreviewImage{xma.PreviewImage, xma.XMAPreviewImage} {
		if preview != nil && preview.PreviewImageDecorationType != "" {
			return preview.PreviewImageDecorationType
		}
	}
	return ""
}

// permalink is a fetched item's public link, or "" when the item has no
// shortcode.
func permalink(item *responses.Items) string {
	if item == nil || item.Code == "" {
		return ""
	}
	kind := "p"
	if item.ProductType == "clips" {
		kind = "reel"
	}
	return shareLink{kind: kind, shortcode: item.Code}.canonical()
}

func isNumericID(id string) bool {
	if id == "" || id == "0" {
		return false
	}
	for _, character := range id {
		if character < '0' || character > '9' {
			return false
		}
	}
	return true
}
