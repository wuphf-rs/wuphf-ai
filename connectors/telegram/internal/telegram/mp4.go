package telegramconnector

import "encoding/binary"

// mp4Info is what an MP4's header says about its video: the first video
// track's size, and the movie's length.
type mp4Info struct {
	width, height int
	seconds       float64
}

// probeMP4 reads an MP4's moov box: mvhd for the duration, and the first
// track whose tkhd has a size (the video, not the audio) for the dimensions.
// It reports false when the data is not an MP4 it can read; nothing is
// decoded, only the box headers.
func probeMP4(data []byte) (mp4Info, bool) {
	moov, ok := mp4Box(data, "moov")
	if !ok {
		return mp4Info{}, false
	}
	var info mp4Info
	if mvhd, ok := mp4Box(moov, "mvhd"); ok {
		info.seconds = mvhdSeconds(mvhd)
	}
	for rest := moov; len(rest) > 0; {
		kind, body, next, ok := nextMP4Box(rest)
		if !ok {
			break
		}
		rest = next
		if kind != "trak" {
			continue
		}
		if tkhd, ok := mp4Box(body, "tkhd"); ok {
			if width, height := tkhdSize(tkhd); width > 0 && height > 0 {
				info.width, info.height = width, height
				break
			}
		}
	}
	return info, info.width > 0 && info.height > 0
}

// mp4Box returns the body of the first box of that kind among data's boxes.
func mp4Box(data []byte, want string) ([]byte, bool) {
	for len(data) > 0 {
		kind, body, next, ok := nextMP4Box(data)
		if !ok {
			return nil, false
		}
		if kind == want {
			return body, true
		}
		data = next
	}
	return nil, false
}

// nextMP4Box splits off the first box: its kind, its body and what follows.
// A size of 1 means a 64-bit size follows the kind; 0 means "to the end".
func nextMP4Box(data []byte) (kind string, body, rest []byte, ok bool) {
	if len(data) < 8 {
		return "", nil, nil, false
	}
	size := uint64(binary.BigEndian.Uint32(data))
	kind = string(data[4:8])
	header := uint64(8)
	switch size {
	case 0:
		size = uint64(len(data))
	case 1:
		if len(data) < 16 {
			return "", nil, nil, false
		}
		size, header = binary.BigEndian.Uint64(data[8:]), 16
	}
	if size < header || size > uint64(len(data)) {
		return "", nil, nil, false
	}
	return kind, data[header:size], data[size:], true
}

// mvhdSeconds is the movie's duration over its timescale (versions 0 and 1).
func mvhdSeconds(mvhd []byte) float64 {
	if len(mvhd) < 1 {
		return 0
	}
	var timescale, duration uint64
	switch mvhd[0] {
	case 0:
		if len(mvhd) < 20 {
			return 0
		}
		timescale = uint64(binary.BigEndian.Uint32(mvhd[12:]))
		duration = uint64(binary.BigEndian.Uint32(mvhd[16:]))
	case 1:
		if len(mvhd) < 32 {
			return 0
		}
		timescale = uint64(binary.BigEndian.Uint32(mvhd[20:]))
		duration = binary.BigEndian.Uint64(mvhd[24:])
	default:
		return 0
	}
	if timescale == 0 {
		return 0
	}
	return float64(duration) / float64(timescale)
}

// tkhdSize is a track's presentation size, stored as 16.16 fixed point after
// the version-dependent times, the layer, volume and the matrix.
func tkhdSize(tkhd []byte) (width, height int) {
	if len(tkhd) < 1 {
		return 0, 0
	}
	// Version 0: version and flags (4), creation, modification, track id,
	// reserved and duration (5×4), reserved (8), layer, group, volume and
	// reserved (8), the matrix (36).
	offset := 76
	if tkhd[0] == 1 {
		offset = 88 // version 1: the times and the duration are 64-bit
	}
	if len(tkhd) < offset+8 {
		return 0, 0
	}
	width = int(binary.BigEndian.Uint32(tkhd[offset:]) >> 16)
	height = int(binary.BigEndian.Uint32(tkhd[offset+4:]) >> 16)
	return width, height
}
