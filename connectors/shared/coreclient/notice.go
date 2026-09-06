package coreclient

import (
	"context"

	wuphfpb "github.com/akshettrj/wuphf/proto/go/connector/v1"
)

// Notice builds an owner-notice frame. A connector that loses its session
// otherwise just goes quiet, which looks identical to "nobody is messaging you";
// the core relays this to the owner through the connectors that still work.
//
// Report only what happened. The wording the owner reads, which chats it
// reaches, and whether a repeat is worth sending are all decided by the core.
func Notice(kind wuphfpb.NoticeKind, detail string) *wuphfpb.ClientFrame {
	return &wuphfpb.ClientFrame{
		Body: &wuphfpb.ClientFrame_OwnerNotice{
			OwnerNotice: &wuphfpb.OwnerNotice{Kind: kind, Detail: detail},
		},
	}
}

// SendNotice queues a notice on the connector's input channel, giving up if the
// context ends or the channel is full. A notice is best-effort by design: it
// must never block or fail the caller, which is usually an event handler on the
// platform library's own goroutine.
func SendNotice(ctx context.Context, inputs chan<- *wuphfpb.ClientFrame, kind wuphfpb.NoticeKind, detail string) {
	select {
	case inputs <- Notice(kind, detail):
	case <-ctx.Done():
	default:
	}
}
