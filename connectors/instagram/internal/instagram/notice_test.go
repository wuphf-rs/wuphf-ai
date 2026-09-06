package instagramconnector

import (
	"errors"
	"strings"
	"testing"

	wuphfpb "github.com/akshettrj/wuphf/proto/go/connector/v1"
)

// A permanent connection failure means messagix has stopped reconnecting, so
// the connector is idle until someone refreshes the session. The owner is told
// that credentials are the fix, not that the bridge is merely degraded.
func TestSessionNoticeAsksForNewCredentials(t *testing.T) {
	t.Parallel()

	kind, detail := sessionNotice(errors.New("connection refused: unauthorized"))
	if kind != wuphfpb.NoticeKind_NOTICE_KIND_AUTH_REQUIRED {
		t.Fatalf("sessionNotice() kind = %v, want auth required", kind)
	}
	// The platform's own reason is the part that says whether new cookies will
	// even help, so it has to survive into the notice.
	if !strings.Contains(detail, "unauthorized") {
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
