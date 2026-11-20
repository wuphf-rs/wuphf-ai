package functions

import (
	"slices"
	"testing"
)

func TestParseInstagramPostLink(t *testing.T) {
	t.Parallel()
	for _, test := range []struct {
		link string
		code string
	}{
		{"https://www.instagram.com/reel/C1a2B3c4D5e/", "C1a2B3c4D5e"},
		{"https://instagram.com/reels/C1a2B3c4D5e", "C1a2B3c4D5e"},
		{"https://www.instagram.com/p/B-x_9yZ/?igsh=abc123", "B-x_9yZ"},
		{"http://www.instagram.com/p/Bxyz/#comments", "Bxyz"},
		{"https://WWW.Instagram.com/reel/Cabc/?utm_source=ig_web_copy_link", "Cabc"},
		{"https://www.instagram.com/some.user/p/Cabc/", "Cabc"},
		{"https://www.instagram.com/stories/someone/3141592653/", ""},
		{"https://www.instagram.com/someone/", ""},
		{"https://www.instagram.com/explore/tags/cats/", ""},
		{"https://www.instagram.com/p/", ""},
		{"https://example.com/p/Cabc/", ""},
		{"https://instagram.com.evil.example/p/Cabc/", ""},
		{"ftp://instagram.com/p/Cabc/", ""},
		{"instagram.com/p/Cabc/", ""},
	} {
		code, ok := ParseInstagramPostLink(test.link)
		if code != test.code || ok != (test.code != "") {
			t.Errorf("ParseInstagramPostLink(%q) = %q, %v; want %q", test.link, code, ok, test.code)
		}
	}
}

func TestInstagramPostLinks(t *testing.T) {
	t.Parallel()
	text := "look (https://www.instagram.com/reel/Cabc/?igsh=x) and https://instagram.com/p/Bdef/. " +
		"again https://www.instagram.com/reels/Cabc/ and a story https://www.instagram.com/stories/u/1/ " +
		"and https://example.com/p/Cxyz/"
	got := InstagramPostLinks(text)
	want := []string{"https://www.instagram.com/reel/Cabc/?igsh=x", "https://instagram.com/p/Bdef/"}
	if !slices.Equal(got, want) {
		t.Errorf("InstagramPostLinks() = %q, want %q", got, want)
	}
	if got := InstagramPostLinks("no links here"); len(got) != 0 {
		t.Errorf("InstagramPostLinks(no links) = %q", got)
	}
}
