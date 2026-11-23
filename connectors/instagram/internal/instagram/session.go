package instagramconnector

import (
	"context"
	"errors"
	"fmt"

	"github.com/rs/zerolog"
	coreclient "github.com/wuphf-rs/wuphf-ai/connectors/shared/coreclient"
	wuphfpb "github.com/wuphf-rs/wuphf-ai/proto/go/connector/v1"
	"go.mau.fi/mautrix-meta/pkg/instameow"
	"go.mau.fi/mautrix-meta/pkg/instameow/slidetypes"
	"go.mau.fi/mautrix-meta/pkg/messagix/dgw"
)

// maxMainStreamFailures is how many consecutive "main stream closed" failures
// are tolerated before the connection is treated as broken rather than
// flaky, matching upstream's bridge.
const maxMainStreamFailures = 5

// session owns the Instagram side of the connector: loading the account,
// keeping the realtime stream connected, and routing its events to the
// normalizer, the sync state and the owner's notices.
type session struct {
	client     *instameow.Client
	threads    *ThreadDirectory
	normalizer *Normalizer
	state      StateStore
	inputs     chan<- *wuphfpb.ClientFrame
	logger     zerolog.Logger

	// resnapshot is signalled when Instagram declares the saved stream
	// position too old to resume; the connection loop then starts afresh.
	resnapshot chan struct{}
}

// load authenticates and loads the account. With resume, it continues the
// delta stream from the saved position so missed deltas are replayed; without
// it (or with nothing saved) it starts from the account's current position.
// Either way the inbox listing primes the thread directory.
func (s *session) load(ctx context.Context, resume bool) (*slidetypes.Mailbox, error) {
	saved, err := s.state.Load()
	if err != nil {
		s.logger.Warn().Err(err).Msg("ignoring unreadable Instagram sync state")
		saved = SyncState{}
	}
	if !resume || saved.SeqID == 0 {
		_, mailbox, err := s.client.LoadIndex(ctx)
		if err != nil {
			return nil, fmt.Errorf("load Instagram account: %w", err)
		}
		return mailbox, nil
	}
	s.logger.Info().Int64("seq_id", saved.SeqID).Time("seq_ts", saved.Timestamp).Msg("resuming Instagram stream from saved position")
	s.client.SetSeqID(saved.SeqID, saved.Timestamp)
	if _, err := s.client.ReloadIndex(ctx); err != nil {
		return nil, fmt.Errorf("load Instagram account: %w", err)
	}
	// The listing only primes titles and ids here; without it they are
	// fetched per thread as deltas arrive, so a failure is not fatal.
	response, err := s.client.GetMailbox(ctx)
	if err != nil {
		s.logger.Warn().Err(err).Msg("failed to list Instagram inbox; thread titles will load lazily")
		return nil, nil
	}
	return response.GetMailbox(), nil
}

// run keeps the realtime stream connected until ctx ends. When Instagram
// stops reconnects for good (an auth failure the owner has been told about),
// it returns quietly and leaves the connector idle: the core session is what
// carries that notice out, so it must stay up.
func (s *session) run(ctx context.Context) {
	for {
		stopped := make(chan struct{})
		go func() {
			defer close(stopped)
			s.client.Connect(ctx)
		}()
		select {
		case <-ctx.Done():
			<-stopped
			return
		case <-stopped:
		}
		select {
		case <-s.resnapshot:
		default:
			s.logger.Error().Msg("Instagram connection stopped; connector is idle until restarted")
			return
		}
		s.logger.Warn().Msg("Instagram asked for a resnapshot; reloading the account (messages from the gap are not replayed)")
		if err := s.state.Clear(); err != nil {
			s.logger.Warn().Err(err).Msg("failed to clear Instagram sync state")
		}
		mailbox, err := s.load(ctx, false)
		if err != nil {
			if ctx.Err() != nil {
				return
			}
			s.logger.Err(err).Msg("Instagram reload failed; connector is idle until restarted")
			kind, detail := sessionNotice(err)
			coreclient.SendNotice(ctx, s.inputs, kind, detail)
			return
		}
		s.threads.RecordMailbox(mailbox)
	}
}

// handleEvent is the instameow event handler. An error returned for a delta
// leaves it unacknowledged, to be redelivered after a reconnect; an error
// returned for a disconnect stops reconnecting.
func (s *session) handleEvent(ctx context.Context, raw slidetypes.ClientEvent) error {
	switch event := raw.(type) {
	case *slidetypes.Delta:
		return s.normalizer.Handle(ctx, event)
	case *slidetypes.Connected:
		s.logger.Info().Int64("seq_id", event.SubscribedSeqID).Int64("latest_seq_id", event.LatestSeqID).Msg("Instagram stream connected")
		// Reaching the platform again tells the owner the bridge is back. The
		// core drops it when nothing was reported wrong, so an ordinary
		// connect stays quiet.
		coreclient.SendNotice(ctx, s.inputs, wuphfpb.NoticeKind_NOTICE_KIND_RECOVERED, "")
		return nil
	case *slidetypes.Disconnected:
		if kind, detail, final := disconnectNotice(event); final {
			s.logger.Err(event.Error).Msg("Instagram connection failed permanently")
			coreclient.SendNotice(ctx, s.inputs, kind, detail)
			return fmt.Errorf("stop reconnecting: %w", event.Error)
		}
		s.logger.Warn().Err(event.Error).Int("failures", event.FailureCount).Msg("Instagram connection dropped, reconnecting")
		return nil
	case *slidetypes.AuthError:
		s.logger.Err(event.Error).Msg("Instagram rejected the session")
		kind, detail := sessionNotice(event.Error)
		coreclient.SendNotice(ctx, s.inputs, kind, detail)
		return nil
	case *slidetypes.SeqIDUpdate:
		if err := s.state.Save(SyncState{SeqID: event.SeqID, Timestamp: event.Timestamp}); err != nil {
			s.logger.Warn().Err(err).Msg("failed to save Instagram sync state")
		}
		return nil
	case *slidetypes.ResnapshotRequired:
		select {
		case s.resnapshot <- struct{}{}:
		default:
		}
		return nil
	default:
		// Typing indicators and cached-config updates are not bridged.
		return nil
	}
}

// disconnectNotice decides whether a dropped connection is final. Instagram
// refusing the session outright needs new credentials; a main stream that
// keeps closing means the connection cannot be kept up. Anything else is
// transient: instameow retries with backoff and it routinely clears on its
// own, so notifying would page the owner about an outage that fixes itself.
func disconnectNotice(event *slidetypes.Disconnected) (wuphfpb.NoticeKind, string, bool) {
	switch {
	case dgw.IsUnauthorized(event.Error), errors.Is(event.Error, instameow.ErrUserIDMissing):
		kind, detail := sessionNotice(event.Error)
		return kind, detail, true
	case event.FailureCount > maxMainStreamFailures && errors.Is(event.Error, instameow.ErrMainStreamClosed):
		return wuphfpb.NoticeKind_NOTICE_KIND_DEGRADED,
			fmt.Sprintf("Instagram keeps closing the realtime stream; restart the connector (%v)", event.Error),
			true
	}
	return 0, "", false
}

// sessionNotice classifies a failure that needs the owner. Instagram refuses
// the session once its cookies are no longer good, or holds it behind a
// challenge or checkpoint, so the fix is fresh cookies (after clearing any
// prompt in the app) rather than anything the connector can retry.
func sessionNotice(err error) (wuphfpb.NoticeKind, string) {
	detail := "Instagram refused the session"
	if err != nil {
		detail = fmt.Sprintf("%s: %v", detail, err)
	}
	return wuphfpb.NoticeKind_NOTICE_KIND_AUTH_REQUIRED, detail
}
