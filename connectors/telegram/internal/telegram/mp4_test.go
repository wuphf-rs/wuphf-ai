package telegramconnector

import (
	"encoding/binary"
	"slices"
	"testing"

	"github.com/gotd/td/tg"
	"github.com/wuphf-rs/wuphf-ai/proto/go/connector/v1"
)

// box is an MP4 box: a 32-bit size, the kind, the body.
func box(kind string, parts ...[]byte) []byte {
	size := 8
	for _, part := range parts {
		size += len(part)
	}
	out := binary.BigEndian.AppendUint32(nil, uint32(size))
	out = append(out, kind...)
	for _, part := range parts {
		out = append(out, part...)
	}
	return out
}

// largeBox is the same box with the 64-bit size form (size field 1).
func largeBox(kind string, body []byte) []byte {
	out := binary.BigEndian.AppendUint32(nil, 1)
	out = append(out, kind...)
	out = binary.BigEndian.AppendUint64(out, uint64(16+len(body)))
	return append(out, body...)
}

func mvhd(version byte, timescale uint32, duration uint64) []byte {
	body := []byte{version, 0, 0, 0}
	if version == 1 {
		body = append(body, make([]byte, 16)...) // creation, modification
		body = binary.BigEndian.AppendUint32(body, timescale)
		body = binary.BigEndian.AppendUint64(body, duration)
	} else {
		body = append(body, make([]byte, 8)...)
		body = binary.BigEndian.AppendUint32(body, timescale)
		body = binary.BigEndian.AppendUint32(body, uint32(duration))
	}
	return box("mvhd", append(body, make([]byte, 80)...)) // rate, volume, matrix, …
}

func tkhd(version byte, width, height uint32) []byte {
	body := []byte{version, 0, 0, 7}
	if version == 1 {
		body = append(body, make([]byte, 32)...) // 64-bit times, id, reserved, duration
	} else {
		body = append(body, make([]byte, 20)...)
	}
	body = append(body, make([]byte, 16+36)...) // reserved, layer…volume, matrix
	body = binary.BigEndian.AppendUint32(body, width<<16)
	body = binary.BigEndian.AppendUint32(body, height<<16)
	return box("tkhd", body)
}

func TestProbeMP4(t *testing.T) {
	t.Parallel()

	ftyp := box("ftyp", []byte("isom\x00\x00\x02\x00isomiso2mp41"))
	mdat := box("mdat", make([]byte, 64))
	audio := box("trak", tkhd(0, 0, 0), box("mdia"))
	cases := []struct {
		name string
		data []byte
		want mp4Info
		ok   bool
	}{
		{
			"the video track's size, past an audio track",
			slices.Concat(ftyp, box("moov", mvhd(0, 1000, 2500), audio, box("trak", tkhd(0, 480, 270))), mdat),
			mp4Info{width: 480, height: 270, seconds: 2.5},
			true,
		},
		{
			"version 1 headers and a moov after mdat",
			slices.Concat(ftyp, mdat, box("moov", mvhd(1, 600, 1800), box("trak", tkhd(1, 320, 568)))),
			mp4Info{width: 320, height: 568, seconds: 3},
			true,
		},
		{
			"64-bit box sizes",
			slices.Concat(ftyp, largeBox("moov", slices.Concat(mvhd(0, 30, 45), largeBox("trak", tkhd(0, 640, 360))))),
			mp4Info{width: 640, height: 360, seconds: 1.5},
			true,
		},
		{"no moov", slices.Concat(ftyp, mdat), mp4Info{}, false},
		{"only audio", box("moov", mvhd(0, 1000, 1000), audio), mp4Info{seconds: 1}, false},
		{"a box larger than the data", append(binary.BigEndian.AppendUint32(nil, 4096), "moov"...), mp4Info{}, false},
		{"not an MP4", []byte("GIF89a\x01\x00\x01\x00"), mp4Info{}, false},
		{"empty", nil, mp4Info{}, false},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			got, ok := probeMP4(tc.data)
			if ok != tc.ok || got != tc.want {
				t.Fatalf("probeMP4() = %+v, %v; want %+v, %v", got, ok, tc.want, tc.ok)
			}
		})
	}
}

func TestAnimationDocumentIsATelegramGIF(t *testing.T) {
	t.Parallel()

	file := &tg.InputFile{ID: 1, Parts: 1, Name: "animation.mp4"}
	data := box("moov", mvhd(0, 1000, 1200), box("trak", tkhd(0, 498, 280)))

	document := animationDocument(file, "animation.mp4", "video/mp4", data)
	if document.MimeType != "video/mp4" || !document.NosoundVideo || document.File != file {
		t.Fatalf("document = %+v; want a silent video/mp4 of the uploaded file", document)
	}
	var name string
	var video *tg.DocumentAttributeVideo
	var animated bool
	for _, attribute := range document.Attributes {
		switch attribute := attribute.(type) {
		case *tg.DocumentAttributeFilename:
			name = attribute.FileName
		case *tg.DocumentAttributeVideo:
			video = attribute
		case *tg.DocumentAttributeAnimated:
			animated = true
		}
	}
	if name != "animation.mp4" || !animated || video == nil {
		t.Fatalf("attributes = %+v; want the filename, video and animated attributes", document.Attributes)
	}
	if video.W != 498 || video.H != 280 || video.Duration != 1.2 || !video.Nosound || !video.SupportsStreaming {
		t.Fatalf("video attribute = %+v; want 498×280, 1.2s, silent, streamable", video)
	}
}

func TestAnimationDocumentKeepsARealGIF(t *testing.T) {
	t.Parallel()

	document := animationDocument(&tg.InputFile{ID: 1}, "animation.gif", "image/gif", []byte("GIF89a"))
	if document.MimeType != "image/gif" {
		t.Fatalf("mime = %q; want image/gif", document.MimeType)
	}
	for _, attribute := range document.Attributes {
		if _, ok := attribute.(*tg.DocumentAttributeVideo); ok {
			t.Fatalf("a GIF image got a video attribute: %+v", document.Attributes)
		}
	}
	if len(document.Attributes) != 2 {
		t.Fatalf("attributes = %+v; want the filename and animated attributes", document.Attributes)
	}
}

func TestGIFUploadName(t *testing.T) {
	t.Parallel()

	name := func(file, mime string) *wuphfpb.Attachment {
		attachment := &wuphfpb.Attachment{Kind: wuphfpb.Attachment_KIND_VIDEO, Animated: true}
		if file != "" {
			attachment.FileName = &file
		}
		if mime != "" {
			attachment.MimeType = &mime
		}
		return attachment
	}
	cases := []struct {
		attachment *wuphfpb.Attachment
		want       string
	}{
		{name("", "video/mp4"), "animation.mp4"},
		{name("", ""), "animation.mp4"},
		{name("", "image/gif"), "animation.gif"},
		{name("party.mp4", "video/mp4"), "party.mp4"},
		{name("noext", "image/gif"), "animation.gif"},
	}
	for _, tc := range cases {
		if got := gifUploadName(tc.attachment); got != tc.want {
			t.Errorf("gifUploadName(%v, %v) = %q; want %q", tc.attachment.FileName, tc.attachment.MimeType, got, tc.want)
		}
	}
}
