package instagramconnector

import (
	"context"
	"encoding/json"
	"errors"
	"io"
	"math/big"
	"net/http"
	"slices"
	"strings"
	"testing"

	"github.com/akshettrj/wuphf/connectors/shared/functions"
	wuphfpb "github.com/akshettrj/wuphf/proto/go/connector/v1"
	"github.com/rs/zerolog"
	"go.mau.fi/mautrix-meta/pkg/messagix/responses"
)

// encodeShortcode is the inverse of MediaIDFromShortcode, written
// independently for the round-trip test.
func encodeShortcode(id *big.Int) string {
	if id.Sign() == 0 {
		return "A"
	}
	var code []byte
	value := new(big.Int).Set(id)
	sixtyFour := big.NewInt(64)
	digit := new(big.Int)
	for value.Sign() > 0 {
		value.DivMod(value, sixtyFour, digit)
		code = append([]byte{shortcodeAlphabet[digit.Int64()]}, code...)
	}
	return string(code)
}

func TestMediaIDFromShortcode(t *testing.T) {
	t.Parallel()
	for _, test := range []struct{ code, id string }{
		{"A", "0"},
		{"B", "1"},
		{"_", "63"},
		{"BA", "64"},
		{"-_", "4031"},
		// 11 characters cover 66 bits: past uint64.
		{"___________", new(big.Int).Sub(new(big.Int).Lsh(big.NewInt(1), 66), big.NewInt(1)).String()},
		// A private account's long code: only the first 11 characters count.
		{"BAAAAAAAAAAxyzXYZ", new(big.Int).Lsh(big.NewInt(1), 60).String()},
	} {
		id, err := MediaIDFromShortcode(test.code)
		if err != nil || id != test.id {
			t.Errorf("MediaIDFromShortcode(%q) = %q, %v; want %q", test.code, id, err, test.id)
		}
	}
	for _, id := range []string{"3141592653589793238", "2620514513487611081", "18014398509481983"} {
		value, _ := new(big.Int).SetString(id, 10)
		got, err := MediaIDFromShortcode(encodeShortcode(value))
		if err != nil || got != id {
			t.Errorf("round trip of %s = %q, %v", id, got, err)
		}
	}
	for _, bad := range []string{"", "C$abc"} {
		if _, err := MediaIDFromShortcode(bad); err == nil {
			t.Errorf("MediaIDFromShortcode(%q) succeeded", bad)
		}
	}
}

// urlCDN serves a body per URL; a URL it does not know fails.
type urlCDN map[string]string

func (c urlCDN) Do(request *http.Request) (*http.Response, error) {
	body, ok := c[request.URL.String()]
	if !ok {
		return nil, errors.New("connection refused")
	}
	return &http.Response{StatusCode: http.StatusOK, Status: "200 OK", Header: http.Header{}, Body: io.NopCloser(strings.NewReader(body))}, nil
}

// postFetcher returns one canned media response and records what was asked.
type postFetcher struct {
	response *responses.FetchMediaResponse
	err      error
	asked    [][2]string
}

func (f *postFetcher) FetchMedia(_ context.Context, mediaID, shortcode string) (*responses.FetchMediaResponse, error) {
	f.asked = append(f.asked, [2]string{mediaID, shortcode})
	return f.response, f.err
}

func videoItem(url string) *responses.Items {
	return &responses.Items{
		VideoVersions:  []responses.VideoVersions{{Width: 360, Height: 640, URL: url + "-small"}, {Width: 720, Height: 1280, URL: url}},
		ImageVersions2: responses.ImageVersions2{Candidates: []responses.Candidates{{Width: 720, Height: 1280, URL: url + "-cover"}}},
	}
}

func imageItem(url string) *responses.Items {
	return &responses.Items{ImageVersions2: responses.ImageVersions2{Candidates: []responses.Candidates{
		{Width: 320, Height: 320, URL: url + "-small"}, {Width: 1080, Height: 1080, URL: url},
	}}}
}

func post(parts ...*responses.Items) *responses.FetchMediaResponse {
	item := &responses.Items{User: responses.User{Username: "someone"}, Caption: responses.Caption{Text: "a caption"}}
	if len(parts) == 1 {
		item.VideoVersions, item.ImageVersions2 = parts[0].VideoVersions, parts[0].ImageVersions2
	} else {
		item.CarouselMedia = parts
	}
	return &responses.FetchMediaResponse{Items: []*responses.Items{item}}
}

func invokeFetch(t *testing.T, fetcher *postFetcher, cdn HTTPDoer, url string) (functions.InstagramFetchMediaResult, []string, *recordingBroker, error) {
	t.Helper()
	broker := &recordingBroker{}
	function := FetchMediaFunction(NewMediaCodec(broker, cdn, fetcher), 2, zerolog.Nop())
	args, _ := json.Marshal(functions.InstagramFetchMediaArgs{URL: url})
	result, err := function.Handle(context.Background(), functions.Call{ID: "c1", Args: args})
	if err != nil {
		return functions.InstagramFetchMediaResult{}, nil, broker, err
	}
	body, ok := result.Body.(functions.InstagramFetchMediaResult)
	if !ok {
		t.Fatalf("result body is %T", result.Body)
	}
	return body, result.MediaIDs, broker, nil
}

func kinds(items []functions.InstagramMediaItem) []string {
	var out []string
	for _, item := range items {
		out = append(out, item.Kind)
	}
	return out
}

func TestFetchMediaReel(t *testing.T) {
	t.Parallel()
	fetcher := &postFetcher{response: post(videoItem("https://cdn/v1"))}
	result, mediaIDs, broker, err := invokeFetch(t, fetcher, urlCDN{"https://cdn/v1": "video"}, "https://www.instagram.com/reel/BA/?igsh=x")
	if err != nil {
		t.Fatal(err)
	}
	if !slices.Equal(fetcher.asked, [][2]string{{"64", "BA"}}) {
		t.Errorf("FetchMedia asked %v", fetcher.asked)
	}
	if !slices.Equal(kinds(result.Items), []string{"video"}) || result.Author != "someone" || result.Caption != "a caption" || result.Skipped != 0 {
		t.Errorf("result = %+v", result)
	}
	if !slices.Equal(mediaIDs, []string{"media-1"}) || string(broker.uploads[0]) != "video" || broker.kinds[0] != wuphfpb.Attachment_KIND_VIDEO {
		t.Errorf("media ids = %v, uploads = %q, kinds = %v", mediaIDs, broker.uploads, broker.kinds)
	}
}

func TestFetchMediaPhotoPost(t *testing.T) {
	t.Parallel()
	fetcher := &postFetcher{response: post(imageItem("https://cdn/i1"))}
	result, mediaIDs, broker, err := invokeFetch(t, fetcher, urlCDN{"https://cdn/i1": "photo"}, "https://instagram.com/p/Bxyz/")
	if err != nil {
		t.Fatal(err)
	}
	if !slices.Equal(kinds(result.Items), []string{"image"}) || len(mediaIDs) != 1 || string(broker.uploads[0]) != "photo" {
		t.Errorf("result = %+v, media ids = %v, uploads = %q", result, mediaIDs, broker.uploads)
	}
}

func TestFetchMediaCarouselKeepsOrder(t *testing.T) {
	t.Parallel()
	fetcher := &postFetcher{response: post(imageItem("https://cdn/a"), videoItem("https://cdn/b"), imageItem("https://cdn/c"))}
	cdn := urlCDN{"https://cdn/a": "A", "https://cdn/b": "B", "https://cdn/c": "C"}
	result, mediaIDs, broker, err := invokeFetch(t, fetcher, cdn, "https://www.instagram.com/p/Bxyz/")
	if err != nil {
		t.Fatal(err)
	}
	if !slices.Equal(kinds(result.Items), []string{"image", "video", "image"}) {
		t.Errorf("kinds = %v", kinds(result.Items))
	}
	if !slices.Equal(mediaIDs, []string{"media-1", "media-2", "media-3"}) {
		t.Errorf("media ids = %v", mediaIDs)
	}
	var uploads []string
	for _, upload := range broker.uploads {
		uploads = append(uploads, string(upload))
	}
	if !slices.Equal(uploads, []string{"A", "B", "C"}) {
		t.Errorf("uploads = %v", uploads)
	}
}

// oversizedCDN serves one body past the download cap and small bodies
// otherwise.
type oversizedCDN struct{ big string }

func (c oversizedCDN) Do(request *http.Request) (*http.Response, error) {
	var body io.Reader = strings.NewReader("small")
	if request.URL.String() == c.big {
		body = io.LimitReader(zeroReader{}, maxDownloadBytes+1)
	}
	return &http.Response{StatusCode: http.StatusOK, Status: "200 OK", Header: http.Header{}, Body: io.NopCloser(body)}, nil
}

type zeroReader struct{}

func (zeroReader) Read(buffer []byte) (int, error) {
	clear(buffer)
	return len(buffer), nil
}

func TestFetchMediaSkipsOversizedItem(t *testing.T) {
	t.Parallel()
	fetcher := &postFetcher{response: post(imageItem("https://cdn/a"), videoItem("https://cdn/huge"))}
	result, mediaIDs, broker, err := invokeFetch(t, fetcher, oversizedCDN{big: "https://cdn/huge"}, "https://www.instagram.com/p/Bxyz/")
	if err != nil {
		t.Fatal(err)
	}
	if result.Skipped != 1 || !slices.Equal(kinds(result.Items), []string{"image"}) || len(mediaIDs) != 1 || len(broker.uploads) != 1 {
		t.Errorf("result = %+v, media ids = %v, uploads = %d", result, mediaIDs, len(broker.uploads))
	}
}

func TestFetchMediaFailures(t *testing.T) {
	t.Parallel()
	cases := []struct {
		name    string
		fetcher *postFetcher
		url     string
		invalid bool
	}{
		{"not a post link", &postFetcher{}, "https://www.instagram.com/stories/u/1/", true},
		{"fetch fails", &postFetcher{err: errors.New("login required")}, "https://www.instagram.com/p/Bxyz/", false},
		{"no items", &postFetcher{response: &responses.FetchMediaResponse{}}, "https://www.instagram.com/p/Bxyz/", false},
		{"every download fails", &postFetcher{response: post(imageItem("https://cdn/gone"))}, "https://www.instagram.com/p/Bxyz/", false},
	}
	for _, test := range cases {
		_, _, broker, err := invokeFetch(t, test.fetcher, urlCDN{}, test.url)
		if err == nil {
			t.Errorf("%s: call succeeded", test.name)
			continue
		}
		var callErr *functions.Error
		isInvalid := errors.As(err, &callErr) && callErr.Code == wuphfpb.CallErrorCode_CALL_ERROR_CODE_INVALID_ARGUMENT
		if isInvalid != test.invalid {
			t.Errorf("%s: error %v, invalid argument = %v; want %v", test.name, err, isInvalid, test.invalid)
		}
		if len(broker.uploads) != 0 {
			t.Errorf("%s: %d uploads", test.name, len(broker.uploads))
		}
	}
}
