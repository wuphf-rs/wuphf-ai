package coreclient

import (
	"slices"
	"testing"

	wuphfpb "github.com/akshettrj/wuphf/proto/go/connector/v1"
)

func TestNoticeWithMediaCarriesItsMedia(t *testing.T) {
	t.Parallel()
	notice := NoticeWithMedia(wuphfpb.NoticeKind_NOTICE_KIND_LINK_REQUIRED, "scan the QR code", []string{"m-1"}).GetOwnerNotice()
	if notice.GetKind() != wuphfpb.NoticeKind_NOTICE_KIND_LINK_REQUIRED || notice.GetDetail() != "scan the QR code" {
		t.Fatalf("notice = %v", notice)
	}
	if !slices.Equal(notice.GetMediaIds(), []string{"m-1"}) {
		t.Fatalf("media ids = %v, want [m-1]", notice.GetMediaIds())
	}
	if ids := Notice(wuphfpb.NoticeKind_NOTICE_KIND_LOGGED_OUT, "").GetOwnerNotice().GetMediaIds(); len(ids) != 0 {
		t.Fatalf("a plain notice carries no media: %v", ids)
	}
}
