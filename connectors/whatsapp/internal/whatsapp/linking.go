package whatsappconnector

import (
	"context"
	"fmt"
	"os"
	"sync"
	"time"

	"github.com/akshettrj/wuphf/connectors/shared/coreclient"
	"github.com/akshettrj/wuphf/connectors/whatsapp/internal/config"
	wuphfpb "github.com/akshettrj/wuphf/proto/go/connector/v1"
	"github.com/mdp/qrterminal/v3"
	"github.com/rs/zerolog"
	"google.golang.org/protobuf/proto"
	"rsc.io/qr"
)

// linkGaps are the gaps after the first, second, third… told code before the
// next one is told; the last repeats.
//
// Every QR code and pair code goes to the terminal, and the owner is told
// through a link-required notice too (the QR code as a picture, or the pair
// code), so a connector can be linked again from wherever the owner reads the
// other connectors. Not every code is told: WhatsApp rotates a QR code every
// 20-60 s and ends a pairing round after about 160 s, and the connector then
// starts another, so a connector nobody links would otherwise post a picture
// a minute for as long as it runs.
var linkGaps = []time.Duration{5 * time.Minute, 15 * time.Minute, 30 * time.Minute, time.Hour}

// pairCodeValidity is how long a pair code lasts: whatsmeow closes the pairing
// websocket when the round's QR codes run out, about 160 s after it opens.
const pairCodeValidity = 160 * time.Second

// linking prints every pairing code and tells the owner about some of them.
// One serves every WhatsApp session of the process, so its schedule spans the
// pairing rounds after one logout.
type linking struct {
	inputs chan<- *wuphfpb.ClientFrame
	media  MediaBroker
	method string
	logger zerolog.Logger

	mu       sync.Mutex
	schedule linkSchedule
	// fresh: the owner asked for a code now, so the next one is told
	// whatever the gap.
	fresh bool
}

func newLinking(inputs chan<- *wuphfpb.ClientFrame, media MediaBroker, method string, logger zerolog.Logger) *linking {
	return &linking{inputs: inputs, media: media, method: method, logger: logger}
}

// QR handles a QR code whatsmeow issued, valid for valid. Pairing by code
// shows none: the QR codes only mark the pairing rounds.
func (l *linking) QR(ctx context.Context, code string, valid time.Duration) {
	if l.method != config.LoginMethodQR {
		return
	}
	fmt.Fprintln(os.Stderr, "Scan WhatsApp QR code:")
	qrterminal.GenerateHalfBlock(code, qrterminal.L, os.Stderr)
	next, due := l.due(time.Now())
	if !due {
		return
	}
	mediaID, err := l.uploadQR(ctx, code)
	if err != nil {
		l.logger.Warn().Err(err).Msg("could not attach the WhatsApp QR code to the owner notice")
		l.tell(ctx, qrUnattachedDetail(next), nil)
		return
	}
	l.tell(ctx, qrDetail(valid, next), []string{mediaID})
}

// pairCodeAsk is a pair code to ask WhatsApp for.
type pairCodeAsk struct {
	// promptPhone makes the phone show its "enter code" prompt.
	promptPhone bool
	// tell is whether the owner is told the code, and next the gap before the
	// next told one.
	tell bool
	next time.Duration
}

// PairCodeAsk reports the pair code a pairing round asks for, when pairing by
// code. Every round asks for one, so a valid code is always in the terminal;
// only a told one prompts the phone, which would otherwise prompt every few
// minutes.
func (l *linking) PairCodeAsk() (pairCodeAsk, bool) {
	if l.method != config.LoginMethodPairCode {
		return pairCodeAsk{}, false
	}
	next, tell := l.due(time.Now())
	return pairCodeAsk{promptPhone: tell, tell: tell, next: next}, true
}

// PairCode handles a pair code issued for the configured phone number for ask.
func (l *linking) PairCode(ctx context.Context, code string, ask pairCodeAsk) {
	fmt.Fprintf(os.Stderr, "WhatsApp pair code (valid for about %d seconds): %s\n", int(pairCodeValidity.Seconds()), code)
	if ask.tell {
		l.tell(ctx, pairCodeDetail(code, pairCodeValidity, ask.next), nil)
	}
}

// Linked starts the schedule afresh for the next logout.
func (l *linking) Linked() {
	l.mu.Lock()
	defer l.mu.Unlock()
	l.schedule = linkSchedule{}
}

func (l *linking) due(now time.Time) (time.Duration, bool) {
	l.mu.Lock()
	defer l.mu.Unlock()
	if l.fresh {
		l.fresh = false
		return l.schedule.tell(now), true
	}
	return l.schedule.due(now)
}

// FreshCode is the owner asking for a code now (the settings panel's "send a
// new link code"): the next one — the next QR code, or a pair code asked for
// at the next QR code — is told at once whatever the gap, and a pair code
// prompts the phone.
func (l *linking) FreshCode() {
	l.mu.Lock()
	defer l.mu.Unlock()
	l.fresh = true
}

// FreshPending reports whether a code the owner asked for is still to come,
// so a pairing round asks for a pair code again.
func (l *linking) FreshPending() bool {
	l.mu.Lock()
	defer l.mu.Unlock()
	return l.fresh
}

func (l *linking) uploadQR(ctx context.Context, code string) (string, error) {
	encoded, err := qr.Encode(code, qr.L)
	if err != nil {
		return "", fmt.Errorf("encode the QR code: %w", err)
	}
	png := encoded.PNG()
	attachment, err := l.media.Upload(ctx, &wuphfpb.MediaMetadata{
		Kind:      wuphfpb.Attachment_KIND_IMAGE,
		FileName:  proto.String("whatsapp-link.png"),
		MimeType:  proto.String("image/png"),
		SizeBytes: proto.Uint64(uint64(len(png))),
	}, png)
	if err != nil {
		return "", fmt.Errorf("upload the QR code: %w", err)
	}
	return attachment.GetMediaId(), nil
}

// tell queues the notice, waiting for room: unlike a health notice, a dropped
// one would leave the owner with no way to link the session.
func (l *linking) tell(ctx context.Context, detail string, mediaIDs []string) {
	select {
	case l.inputs <- coreclient.NoticeWithMedia(wuphfpb.NoticeKind_NOTICE_KIND_LINK_REQUIRED, detail, mediaIDs):
	case <-ctx.Done():
	}
}

// linkSchedule records when the owner was last told about a code, and how
// many times.
type linkSchedule struct {
	told int
	last time.Time
}

// due reports whether a code issued at now is told, recording it when it is,
// and if so the gap before the next told code.
func (s *linkSchedule) due(now time.Time) (time.Duration, bool) {
	if s.told > 0 && now.Sub(s.last) < linkGaps[min(s.told-1, len(linkGaps)-1)] {
		return 0, false
	}
	return s.tell(now), true
}

// tell records a code told at now, whatever the gap, and returns the gap
// before the next told code.
func (s *linkSchedule) tell(now time.Time) time.Duration {
	s.told++
	s.last = now
	return linkGaps[min(s.told-1, len(linkGaps)-1)]
}

func qrDetail(valid, next time.Duration) string {
	return fmt.Sprintf("scan the attached QR code in WhatsApp → Settings → Linked devices → Link a device. "+
		"It is valid for %s; if it expires first, a new one follows in about %s.", humanDuration(valid), humanDuration(next))
}

func qrUnattachedDetail(next time.Duration) string {
	return fmt.Sprintf("the QR code to scan is in the connector's terminal (it could not be attached here); "+
		"a new one follows in about %s.", humanDuration(next))
}

func pairCodeDetail(code string, valid, next time.Duration) string {
	return fmt.Sprintf("in WhatsApp → Settings → Linked devices → Link a device → Link with phone number "+
		"instead, enter %s. It is valid for %s; if it expires first, a new one follows in about %s.",
		code, humanDuration(valid), humanDuration(next))
}

// humanDuration reads seconds under two minutes, minutes under an hour, then
// hours.
func humanDuration(d time.Duration) string {
	seconds := int(d.Seconds())
	switch {
	case seconds < 120:
		return fmt.Sprintf("%d s", seconds)
	case seconds < 3600:
		return fmt.Sprintf("%d min", seconds/60)
	default:
		return fmt.Sprintf("%d h", seconds/3600)
	}
}
