package whatsappconnector

import (
	"testing"
	"time"

	wuphfpb "github.com/akshettrj/wuphf/proto/go/connector/v1"
	"go.mau.fi/whatsmeow/types/events"
)

func TestSessionNoticeMapsSessionLoss(t *testing.T) {
	t.Parallel()

	cases := []struct {
		name     string
		event    any
		want     wuphfpb.NoticeKind
		reported bool
	}{
		{
			"a logout needs the owner to re-pair",
			&events.LoggedOut{},
			wuphfpb.NoticeKind_NOTICE_KIND_LOGGED_OUT, true,
		},
		{
			"a replaced stream leaves this session dead too",
			&events.StreamReplaced{},
			wuphfpb.NoticeKind_NOTICE_KIND_LOGGED_OUT, true,
		},
		{
			"an outdated client needs an upgrade, not a re-pair",
			&events.ClientOutdated{},
			wuphfpb.NoticeKind_NOTICE_KIND_AUTH_REQUIRED, true,
		},
		{
			"a temporary ban still bridges, badly",
			&events.TemporaryBan{Expire: time.Hour},
			wuphfpb.NoticeKind_NOTICE_KIND_DEGRADED, true,
		},
		{
			"connecting reports health so a recovery can be noticed",
			&events.Connected{},
			wuphfpb.NoticeKind_NOTICE_KIND_RECOVERED, true,
		},
		{
			// Ordinary traffic must not be mistaken for a health report.
			"an unrelated event is not a notice",
			&events.Receipt{},
			wuphfpb.NoticeKind_NOTICE_KIND_UNSPECIFIED, false,
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			kind, _, ok := sessionNotice(tc.event)
			if ok != tc.reported {
				t.Fatalf("sessionNotice() reported = %v, want %v", ok, tc.reported)
			}
			if kind != tc.want {
				t.Fatalf("sessionNotice() kind = %v, want %v", kind, tc.want)
			}
		})
	}
}

// A rejected connect carries WhatsApp's own reason, which is the part that tells
// the owner whether re-pairing will even help.
func TestSessionNoticeCarriesTheConnectFailureReason(t *testing.T) {
	t.Parallel()
	_, detail, ok := sessionNotice(&events.LoggedOut{OnConnect: true})
	if !ok || detail == "" {
		t.Fatalf("sessionNotice() = %q, %v; want a detail explaining the rejection", detail, ok)
	}
}
