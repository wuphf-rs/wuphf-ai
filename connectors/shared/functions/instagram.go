// The instagram.fetch_media@1 contract, shared by its provider (the Instagram
// connector) and its callers (wuphf-misc), so both sides encode the same
// bodies and agree on which links are posts.

package functions

import (
	"net/url"
	"regexp"
	"strings"
)

const (
	InstagramFetchMediaName  = "instagram.fetch_media"
	InstagramFetchMediaMajor = 1
)

// InstagramFetchMediaArgs are the args of instagram.fetch_media@1.
type InstagramFetchMediaArgs struct {
	// URL is a post or reel link (see ParseInstagramPostLink).
	URL string `json:"url"`
}

// InstagramFetchMediaResult is the result of instagram.fetch_media@1. The
// downloaded media are the result's media ids, in Items order.
type InstagramFetchMediaResult struct {
	Items []InstagramMediaItem `json:"items"`
	// Author is the poster's username.
	Author string `json:"author,omitempty"`
	// Caption is the post's full text.
	Caption string `json:"caption,omitempty"`
	// Skipped counts the post's media left out (too large, or a download
	// that failed).
	Skipped int `json:"skipped,omitempty"`
}

// InstagramMediaItem describes one downloaded medium.
type InstagramMediaItem struct {
	// Kind is "video" or "image".
	Kind string `json:"kind"`
}

// Kinds of InstagramMediaItem.
const (
	InstagramMediaVideo = "video"
	InstagramMediaImage = "image"
)

// instagramPostPath matches a post or reel path: /p/<code>, /reel/<code>,
// /reels/<code>. A /<user>/p/<code> path (the form the app shares for some
// posts) matches too.
var instagramPostPath = regexp.MustCompile(`^(?:/[\w.]+)?/(?:p|reels?)/([\w-]+)/?$`)

// ParseInstagramPostLink returns the shortcode of an instagram.com post or
// reel link. Query and fragment are ignored. Stories, profiles and other hosts
// are not post links.
func ParseInstagramPostLink(raw string) (shortcode string, ok bool) {
	link, err := url.Parse(strings.TrimSpace(raw))
	if err != nil || (link.Scheme != "http" && link.Scheme != "https") {
		return "", false
	}
	host := strings.ToLower(link.Hostname())
	if host != "instagram.com" && host != "www.instagram.com" {
		return "", false
	}
	match := instagramPostPath.FindStringSubmatch(link.Path)
	if match == nil {
		return "", false
	}
	return match[1], true
}

// instagramLinkCandidate finds URL-like words that mention Instagram; each is
// then checked with ParseInstagramPostLink.
var instagramLinkCandidate = regexp.MustCompile(`(?i)https?://(?:www\.)?instagram\.com/[^\s<>"']+`)

// InstagramPostLinks returns the post and reel links in text, in order, one
// per shortcode.
func InstagramPostLinks(text string) []string {
	var links []string
	seen := map[string]bool{}
	for _, candidate := range instagramLinkCandidate.FindAllString(text, -1) {
		candidate = strings.TrimRight(candidate, ".,;:!?)]}")
		code, ok := ParseInstagramPostLink(candidate)
		if !ok || seen[code] {
			continue
		}
		seen[code] = true
		links = append(links, candidate)
	}
	return links
}
