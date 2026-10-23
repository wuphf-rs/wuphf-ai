package mediaconv

import (
	"bytes"
	"compress/gzip"
	"testing"
)

// TestTGSToAnimatedWebP rasterizes a minimal Lottie animation and checks the
// result is an animated WebP, the form a WhatsApp sticker requires.
func TestTGSToAnimatedWebP(t *testing.T) {
	t.Parallel()

	out, err := TGSToAnimatedWebP(sampleTGS(t))
	if err != nil {
		t.Fatalf("TGSToAnimatedWebP() error = %v", err)
	}
	if !isAnimatedWebP(out) {
		t.Fatalf("output is not an animated WebP (%d bytes)", len(out))
	}
}

// sampleTGS returns a minimal valid .tgs: a short Lottie animation, gzipped.
func sampleTGS(t *testing.T) []byte {
	t.Helper()
	// A single red circle that slides across the frame, so successive frames
	// differ and the encoder keeps them all (an animated, not static, WebP).
	const lottie = `{"v":"5.5.2","fr":30,"ip":0,"op":15,"w":64,"h":64,"nm":"t","ddd":0,"assets":[],"layers":[` +
		`{"ddd":0,"ind":1,"ty":4,"nm":"s","sr":1,"ks":{"o":{"a":0,"k":100},"r":{"a":0,"k":0},` +
		`"p":{"a":1,"k":[{"t":0,"s":[10,32,0],"i":{"x":[0.5],"y":[0.5]},"o":{"x":[0.5],"y":[0.5]}},{"t":15,"s":[54,32,0]}]},` +
		`"a":{"a":0,"k":[0,0,0]},"s":{"a":0,"k":[100,100,100]}},` +
		`"shapes":[{"ty":"el","p":{"a":0,"k":[0,0]},"s":{"a":0,"k":[20,20]}},` +
		`{"ty":"fl","c":{"a":0,"k":[1,0,0,1]},"o":{"a":0,"k":100}}],"ip":0,"op":15,"st":0}]}`
	var buf bytes.Buffer
	gz := gzip.NewWriter(&buf)
	if _, err := gz.Write([]byte(lottie)); err != nil {
		t.Fatal(err)
	}
	if err := gz.Close(); err != nil {
		t.Fatal(err)
	}
	return buf.Bytes()
}
