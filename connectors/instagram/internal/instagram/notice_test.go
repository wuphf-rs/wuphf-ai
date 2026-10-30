package instagramconnector

import (
	"errors"
	"fmt"
	"strings"
	"testing"

	wuphfpb "github.com/akshettrj/wuphf/proto/go/connector/v1"
	"go.mau.fi/mautrix-meta/pkg/instameow"
	"go.mau.fi/mautrix-meta/pkg/instameow/slidetypes"
	"go.mau.fi/mautrix-meta/pkg/messagix/dgw"
)

// A refused session means Instagram will not take these cookies again, so the
// owner is told that credentials are the fix, not that the bridge is merely
// degraded.
func TestSessionNoticeAsksForNewCredentials(t *testing.T) {
	t.Parallel()

	kind, detail := sessionNotice(errors.New("challenge required"))
	if kind != wuphfpb.NoticeKind_NOTICE_KIND_AUTH_REQUIRED {
		t.Fatalf("sessionNotice() kind = %v, want auth required", kind)
	}
	// The platform's own reason is the part that says whether new cookies will
	// even help, so it has to survive into the notice.
	if !strings.Contains(detail, "challenge required") {
		t.Fatalf("sessionNotice() detail = %q, want the platform reason", detail)
	}
}

func TestSessionNoticeWithoutAReasonStillExplains(t *testing.T) {
	t.Parallel()

	kind, detail := sessionNotice(nil)
	if kind != wuphfpb.NoticeKind_NOTICE_KIND_AUTH_REQUIRED || detail == "" {
		t.Fatalf("sessionNotice(nil) = %v, %q; want a usable notice", kind, detail)
	}
}

func TestDisconnectNoticeOnlyForFinalFailures(t *testing.T) {
	t.Parallel()

	cases := []struct {
		name     string
		event    slidetypes.Disconnected
		wantKind wuphfpb.NoticeKind
		final    bool
	}{
		{"unauthorized", slidetypes.Disconnected{Error: fmt.Errorf("dial: %w", dgw.ErrUnauthorized)}, wuphfpb.NoticeKind_NOTICE_KIND_AUTH_REQUIRED, true},
		{"user id missing", slidetypes.Disconnected{Error: instameow.ErrUserIDMissing}, wuphfpb.NoticeKind_NOTICE_KIND_AUTH_REQUIRED, true},
		{"stream keeps closing", slidetypes.Disconnected{Error: instameow.ErrMainStreamClosed, FailureCount: maxMainStreamFailures + 1}, wuphfpb.NoticeKind_NOTICE_KIND_DEGRADED, true},
		// Transient drops clear on their own; paging the owner would be noise.
		{"stream closed once", slidetypes.Disconnected{Error: instameow.ErrMainStreamClosed, FailureCount: 1}, 0, false},
		{"network blip", slidetypes.Disconnected{Error: errors.New("connection reset")}, 0, false},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			kind, detail, final := disconnectNotice(&tc.event)
			if final != tc.final || kind != tc.wantKind || (final && detail == "") {
				t.Fatalf("disconnectNotice() = %v, %q, %v; want %v, final %v", kind, detail, final, tc.wantKind, tc.final)
			}
		})
	}
}
