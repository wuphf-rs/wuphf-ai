package whatsappconnector

import (
	"bytes"
	"context"
	"errors"
	"slices"
	"testing"
	"time"

	"github.com/akshettrj/wuphf/connectors/shared/coreclient"
	"github.com/akshettrj/wuphf/connectors/whatsapp/internal/config"
	wuphfpb "github.com/akshettrj/wuphf/proto/go/connector/v1"
	"github.com/rs/zerolog"
)

func TestLinkCodesAreToldWithGrowingGaps(t *testing.T) {
	t.Parallel()
	start := time.Now()
	at := func(seconds int) time.Time { return start.Add(time.Duration(seconds) * time.Second) }
	var schedule linkSchedule
	steps := []struct {
		seconds int
		due     bool
		next    time.Duration
	}{
		// The first code at once, then nothing for five minutes.
		{0, true, linkGaps[0]},
		{20, false, 0},
		{299, false, 0},
		{300, true, linkGaps[1]},
		// Then fifteen minutes, thirty, and an hour from then on.
		{300 + 899, false, 0},
		{300 + 900, true, linkGaps[2]},
		{1200 + 1800, true, linkGaps[3]},
		{3000 + 3600, true, linkGaps[3]},
		{6600 + 3599, false, 0},
		{6600 + 3600, true, linkGaps[3]},
	}
	for _, step := range steps {
		next, due := schedule.due(at(step.seconds))
		if due != step.due || next != step.next {
			t.Fatalf("due at %d s = (%v, %v), want (%v, %v)", step.seconds, next, due, step.next, step.due)
		}
	}
}

func TestAQRCodeIsToldWithItsPicture(t *testing.T) {
	t.Parallel()
	inputs := make(chan *wuphfpb.ClientFrame, 4)
	broker := &uploadBroker{}
	linking := newLinking(inputs, broker, config.LoginMethodQR, zerolog.Nop())
	linking.QR(context.Background(), "2@abc,def,ghi", time.Minute)
	linking.QR(context.Background(), "2@jkl,mno,pqr", 20*time.Second)

	notice := (<-inputs).GetOwnerNotice()
	if notice.GetKind() != wuphfpb.NoticeKind_NOTICE_KIND_LINK_REQUIRED {
		t.Fatalf("kind = %v", notice.GetKind())
	}
	if !slices.Equal(notice.GetMediaIds(), []string{"media-id"}) {
		t.Fatalf("media ids = %v", notice.GetMediaIds())
	}
	if want := qrDetail(time.Minute, linkGaps[0]); notice.GetDetail() != want {
		t.Fatalf("detail = %q, want %q", notice.GetDetail(), want)
	}
	if !bytes.HasPrefix(broker.bytes, []byte("\x89PNG")) || broker.metadata.GetMimeType() != "image/png" {
		t.Fatalf("uploaded %q as %v, want a PNG", broker.bytes[:min(8, len(broker.bytes))], broker.metadata)
	}
	// The second code falls inside the gap.
	select {
	case frame := <-inputs:
		t.Fatalf("a second notice inside the gap: %v", frame)
	default:
	}

	// A link starts the schedule afresh.
	linking.Linked()
	linking.QR(context.Background(), "2@abc,def,ghi", time.Minute)
	if len(inputs) != 1 {
		t.Fatalf("after a link the next code is told: %d notices", len(inputs))
	}
}

func TestEveryRoundAsksForAPairCodeButOnlyAToldOnePrompts(t *testing.T) {
	t.Parallel()
	inputs := make(chan *wuphfpb.ClientFrame, 4)
	qrMode := newLinking(inputs, &uploadBroker{}, config.LoginMethodQR, zerolog.Nop())
	if _, ok := qrMode.PairCodeAsk(); ok {
		t.Fatal("a QR-mode connector asks for no pair code")
	}
	codeMode := newLinking(inputs, &uploadBroker{}, config.LoginMethodPairCode, zerolog.Nop())
	first, ok := codeMode.PairCodeAsk()
	if want := (pairCodeAsk{promptPhone: true, tell: true, next: linkGaps[0]}); !ok || first != want {
		t.Fatalf("first round PairCodeAsk() = (%+v, %v), want (%+v, true)", first, ok, want)
	}
	// The next round, inside the gap, still asks for a code, quietly.
	quiet, ok := codeMode.PairCodeAsk()
	if !ok || quiet.promptPhone || quiet.tell {
		t.Fatalf("second round PairCodeAsk() = (%+v, %v), want an untold, unprompted ask", quiet, ok)
	}
	codeMode.PairCode(context.Background(), "ABCD-EFGH", first)
	notice := (<-inputs).GetOwnerNotice()
	want := "in WhatsApp → Settings → Linked devices → Link a device → Link with phone number instead, " +
		"enter ABCD-EFGH. It is valid for 2 min; if it expires first, a new one follows in about 5 min."
	if notice.GetDetail() != want || len(notice.GetMediaIds()) != 0 {
		t.Fatalf("notice = %v, want detail %q and no media", notice, want)
	}
	// An untold code goes to the terminal only, and so do no QR codes.
	codeMode.PairCode(context.Background(), "WXYZ-1234", quiet)
	codeMode.Linked()
	codeMode.QR(context.Background(), "2@abc,def,ghi", time.Minute)
	if len(inputs) != 0 {
		t.Fatalf("nothing more is told: %d notices", len(inputs))
	}
}

func TestADispatchWithoutALinkedSessionIsRetried(t *testing.T) {
	t.Parallel()
	var linked linkedSender
	_, _, err := linked.SendMessage(context.Background(), &wuphfpb.MessageCreateDispatch{})
	if !errors.Is(err, errNotLinked) {
		t.Fatalf("SendMessage() error = %v, want errNotLinked", err)
	}
	var permanent coreclient.PermanentError
	if errors.As(err, &permanent) {
		t.Fatal("an unlinked session must not dead-letter the dispatch")
	}
}

func TestDurationsReadNaturally(t *testing.T) {
	t.Parallel()
	for duration, want := range map[time.Duration]string{
		time.Minute:      "60 s",
		5 * time.Minute:  "5 min",
		59 * time.Minute: "59 min",
		time.Hour:        "1 h",
		2 * time.Hour:    "2 h",
	} {
		if got := humanDuration(duration); got != want {
			t.Fatalf("humanDuration(%v) = %q, want %q", duration, got, want)
		}
	}
}
