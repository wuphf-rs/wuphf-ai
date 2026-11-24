package whatsappconnector

import (
	"context"
	"errors"
	"fmt"
	"os"
	"time"

	_ "github.com/mattn/go-sqlite3"
	"github.com/rs/zerolog"
	coreclient "github.com/wuphf-rs/wuphf-ai/connectors/shared/coreclient"
	"github.com/wuphf-rs/wuphf-ai/connectors/shared/functions"
	"github.com/wuphf-rs/wuphf-ai/connectors/shared/logging"
	"github.com/wuphf-rs/wuphf-ai/connectors/shared/sessionlock"
	"github.com/wuphf-rs/wuphf-ai/connectors/shared/settings"
	"github.com/wuphf-rs/wuphf-ai/connectors/whatsapp/internal/config"
	wuphfpb "github.com/wuphf-rs/wuphf-ai/proto/go/connector/v1"
	"go.mau.fi/whatsmeow"
	"go.mau.fi/whatsmeow/store"
	"go.mau.fi/whatsmeow/store/sqlstore"
	"go.mau.fi/whatsmeow/types/events"
	waLog "go.mau.fi/whatsmeow/util/log"
)

type Connector struct {
	cfg    config.File
	logger zerolog.Logger
}

func New(cfg config.File, logger zerolog.Logger) Connector {
	return Connector{cfg: cfg, logger: logger}
}

// errLoggedOut ends a WhatsApp session that WhatsApp logged out; the next one
// links a new device.
var errLoggedOut = errors.New("WhatsApp logged the device out")

// sessionDeps is what every WhatsApp session of the process shares.
type sessionDeps struct {
	container *sqlstore.Container
	media     *coreclient.MediaClient
	converter functions.MediaConverter
	runtime   *settings.Connection[config.Runtime]
	inputs    chan *wuphfpb.ClientFrame
	linked    *linkedSender
	linking   *linking
	// client is the linked session's WhatsApp client, for the commands.
	client *linkedClient
}

// Run runs the connector: one core session for the process, and a WhatsApp
// session that is replaced whenever WhatsApp logs the device out. Dispatches
// that arrive while no session is linked fail as transient, so the core
// delivers them once the new one is.
func (c Connector) Run(ctx context.Context) error {
	// Held while the connector runs, so setup's actions never use the session
	// at the same time.
	lock, err := sessionlock.Acquire(c.cfg.SessionFile())
	if err != nil {
		return fmt.Errorf("lock the WhatsApp session (is another wuphf-whatsapp, or its setup, using it?): %w", err)
	}
	defer lock.Release()
	dbLog := waLog.Zerolog(c.logger.With().Str("component", "whatsmeow-db").Logger())
	container, err := sqlstore.New(ctx, "sqlite3", c.cfg.SessionDatabase, dbLog)
	if err != nil {
		return fmt.Errorf("open WhatsApp session database: %w", err)
	}
	defer container.Close()
	// Register as an Android client (experimental) so WhatsApp delivers
	// view-once media to this companion; must be set before pairing, which a
	// session may start. A WhatsApp Business account pairs as the SMB Android
	// client instead.
	mode := c.cfg.ResolvedClientMode()
	c.logger.Warn().Str("client_mode", mode).Str("login_method", c.cfg.ResolvedLoginMethod()).
		Msg("registering WhatsApp session as an Android client (experimental)")
	useAndroidClientIdentity(mode == config.ClientModeAndroidBusiness)
	coreToken, err := coreclient.TokenFromEnv(c.cfg.CoreTokenEnv)
	if err != nil {
		return err
	}
	media, err := coreclient.DialMedia(c.cfg.CoreAddress, coreToken)
	if err != nil {
		return err
	}
	defer media.Close()
	var functionsToken string
	if c.cfg.FunctionsTokenEnv != "" {
		functionsToken = os.Getenv(c.cfg.FunctionsTokenEnv)
	}
	fns, err := functions.Dial(c.cfg.CoreAddress, functionsToken, c.cfg.ConnectorID)
	if err != nil {
		return err
	}
	defer fns.Close()
	// The runtime settings: declared to the core when registering, then
	// followed as the core pushes changes.
	runtime, err := settings.Connect(c.cfg.CoreAddress, coreToken, c.cfg.ConnectorID, c.cfg.Runtime.WithDefaults(), c.logger)
	if err != nil {
		return err
	}
	defer runtime.Close()
	go runtime.Follow(ctx, func(old, new config.Runtime) {
		if old.LogLevel != new.LogLevel {
			logging.SetLevel(new.LogLevel)
		}
	})
	converter := functions.MediaConverter{Functions: fns, Media: media, TimeoutFunc: func() time.Duration {
		return runtime.Live.Load().MediaConvertTimeout()
	}}
	inputs := make(chan *wuphfpb.ClientFrame, 128)
	deps := sessionDeps{
		container: container,
		media:     media,
		converter: converter,
		runtime:   runtime,
		inputs:    inputs,
		linked:    &linkedSender{},
		linking:   newLinking(inputs, media, c.cfg.ResolvedLoginMethod(), c.logger),
		client:    &linkedClient{},
	}

	// The core session starts first, so the codes of a pairing reach the owner.
	sessionCtx, stopSessions := context.WithCancel(ctx)
	defer stopSessions()
	runResult := make(chan error, 1)
	go func() {
		runResult <- coreclient.New(coreclient.Options{
			CoreAddress:                c.cfg.CoreAddress,
			Token:                      coreToken,
			ConnectorID:                c.cfg.ConnectorID,
			Platform:                   wuphfpb.Platform_PLATFORM_WHATSAPP,
			SupportsThreads:            c.cfg.SupportsThreads,
			SupportsTopicCreate:        c.cfg.SupportsTopicCreate,
			SupportsDisappearingTimers: c.cfg.SupportsDisappearingTimers,
			Settings:                   runtime.Declaration,
			// The settings panel's buttons on this connector's section.
			Commands: newCommands(ctx, deps.client.fetcher, deps.linking, c.logger),
			Prober:   newProber(deps.client),
			Tools:    tools,
		}, c.logger).Run(ctx, deps.linked, inputs)
	}()
	sessionResult := make(chan error, 1)
	go func() { sessionResult <- c.runSessions(sessionCtx, deps) }()
	select {
	case <-ctx.Done():
		<-sessionResult
		return ctx.Err()
	case err := <-runResult:
		stopSessions()
		<-sessionResult
		return err
	case err := <-sessionResult:
		return err
	}
}

// runSessions runs WhatsApp sessions back to back while each ends in a logout.
func (c Connector) runSessions(ctx context.Context, deps sessionDeps) error {
	for {
		err := c.runSession(ctx, deps)
		if !errors.Is(err, errLoggedOut) {
			return err
		}
		c.logger.Warn().Msg("WhatsApp logged this device out; linking a new one")
	}
}

// runSession runs one WhatsApp session on the session database's device,
// linking a new device first when there is none, until ctx ends, an event
// cannot be normalized, or WhatsApp logs the device out (errLoggedOut).
func (c Connector) runSession(ctx context.Context, deps sessionDeps) error {
	device, err := deps.container.GetFirstDevice(ctx)
	if err != nil {
		return fmt.Errorf("load WhatsApp device: %w", err)
	}
	client := whatsmeow.NewClient(device, waLog.Zerolog(c.logger.With().Str("component", "whatsmeow").Logger()))
	inputs := deps.inputs
	identity := newIdentityResolver(client)
	addresses := newAddressBook(identity, whatsmeowAccount{client: client})
	namer := newChatNamer(client)
	profiles := newChatProfiler(client, httpFetch, deps.media, namer, c.logger)
	normalizer := NewNormalizer(inputs, NewMediaCodec(deps.media, client), namer, identity, client, addresses).WithProfiles(profiles)
	eventErrors := make(chan error, 1)
	loggedOut := make(chan struct{}, 1)
	client.AddEventHandler(func(raw any) {
		var err error
		switch evt := raw.(type) {
		case *events.Message:
			err = normalizer.Handle(ctx, evt)
		case *events.Receipt:
			err = normalizer.HandleReceipt(ctx, evt)
		case *events.GroupInfo:
			err = normalizer.HandleGroupInfo(ctx, evt)
		case *events.HistorySync:
			err = normalizer.HandleHistorySync(ctx, evt)
		case *events.Picture:
			err = normalizer.HandlePicture(ctx, evt)
		case *events.Contact:
			err = normalizer.HandleNameChange(ctx, evt.JID, evt.FromFullSync)
		case *events.PushName:
			err = normalizer.HandleNameChange(ctx, evt.JID, false)
		case *events.BusinessName:
			err = normalizer.HandleNameChange(ctx, evt.JID, false)
		case *events.Pin:
			err = normalizer.HandlePin(ctx, evt)
		case *events.Mute:
			err = normalizer.HandleMute(ctx, evt)
		case *events.Archive:
			err = normalizer.HandleArchive(ctx, evt)
		default:
			// Session-health events do not fail the connector: whatsmeow stops
			// delivering messages but the core session stays up, which is what
			// carries the notice out through the other connectors. Staying
			// alive also means a recovery can be reported on the same session.
			if kind, detail, ok := sessionNotice(raw); ok {
				c.logger.Warn().Str("detail", detail).Msg("WhatsApp session health changed")
				coreclient.SendNotice(ctx, inputs, kind, detail)
			}
			switch raw.(type) {
			case *events.LoggedOut:
				// Only a logout links again: a replaced stream, a ban or an
				// outdated client would fail the same way with a new device.
				select {
				case loggedOut <- struct{}{}:
				default:
				}
			case *events.Connected:
				deps.linking.Linked()
			}
			return
		}
		if err != nil {
			select {
			case eventErrors <- err:
			default:
			}
		}
	})
	if err := c.link(ctx, client, deps.linking); err != nil {
		return err
	}
	defer client.Disconnect()
	if client.Store.ID == nil {
		// whatsmeow deletes the device when WhatsApp rejects it on connect.
		return errLoggedOut
	}
	ownJID := *client.Store.ID
	c.logger.Info().Str("jid", ownJID.String()).Msg("WhatsApp session connected")
	sender := NewSender(client, deps.media, deps.converter, addresses, deps.runtime.Live)
	if c.cfg.SupportsDisappearingTimers {
		sender = sender.WithTimers(newTimerSource(client, inputs, ctx))
	}
	deps.linked.set(&sender)
	defer deps.linked.set(nil)
	deps.client.set(client)
	defer deps.client.set(nil)
	select {
	case <-ctx.Done():
		return ctx.Err()
	case err := <-eventErrors:
		return fmt.Errorf("normalize WhatsApp event: %w", err)
	case <-loggedOut:
		client.Disconnect()
		// whatsmeow deletes the device itself, but on its own goroutine;
		// deleting the row here too (an idempotent DELETE) makes sure the next
		// session starts from a fresh device rather than the dead one.
		if err := deps.container.DeleteDevice(ctx, &store.Device{ID: &ownJID}); err != nil {
			return fmt.Errorf("forget the logged-out WhatsApp device: %w", err)
		}
		return errLoggedOut
	}
}

// link connects the session's device, or links a new one by the configured
// login method, one pairing round after another until one succeeds.
func (c Connector) link(ctx context.Context, client *whatsmeow.Client, sink pairCodes) error {
	if client.Store.ID != nil {
		if err := client.Connect(); err != nil {
			return fmt.Errorf("connect WhatsApp session: %w", err)
		}
		return nil
	}
	for {
		linked, err := c.pairRound(ctx, client, sink)
		if err != nil || linked {
			return err
		}
		// The round's codes ran out before one was used; whatsmeow closed the
		// pairing socket, and the next round opens a new one.
		client.Disconnect()
	}
}

// pairRound runs one pairing round: whatsmeow's QR codes, and in pair-code
// mode the round's pair code. It reports whether the device was linked.
// whatsmeow has no event for WhatsApp asking for a new code, so a code it no
// longer takes is replaced by the next round's.
func (c Connector) pairRound(ctx context.Context, client *whatsmeow.Client, sink pairCodes) (bool, error) {
	codes, err := client.GetQRChannel(ctx)
	if err != nil {
		return false, fmt.Errorf("open WhatsApp pairing: %w", err)
	}
	if err := client.Connect(); err != nil {
		return false, fmt.Errorf("connect WhatsApp for pairing: %w", err)
	}
	asked := false
	for code := range codes {
		switch code.Event {
		case whatsmeow.QRChannelEventCode:
			// A pair code is asked for once the round's socket is up (the
			// first QR code shows it is).
			// The owner asking for a code now asks for one again.
			if !asked || sink.FreshPending() {
				asked = true
				if ask, ok := sink.PairCodeAsk(); ok {
					pairCode, err := client.PairPhone(ctx, c.cfg.PhoneNumber, ask.promptPhone, whatsmeow.PairClientChrome, "Chrome (Linux)")
					if err != nil {
						sink.PairCodeFailed(err)
					} else {
						sink.PairCode(ctx, pairCode, ask)
					}
				}
			}
			sink.QR(ctx, code.Code, code.Timeout)
		case whatsmeow.QRChannelSuccess.Event:
			return true, nil
		case whatsmeow.QRChannelTimeout.Event:
			return false, nil
		case whatsmeow.QRChannelEventPasskeyRequest, whatsmeow.QRChannelEventPasskeyResponse:
			c.logger.Info().Str("event", code.Event).Msg("WhatsApp pairing step")
		default:
			return false, fmt.Errorf("WhatsApp pairing failed: %s", code.Event)
		}
	}
	if err := ctx.Err(); err != nil {
		return false, err
	}
	return false, errors.New("WhatsApp pairing stopped before success")
}

// sessionNotice maps the whatsmeow events that stop a session from bridging to
// an owner notice, reporting whether the event is one of them.
//
// Without this the connector stays running and silent after a logout: whatsmeow
// simply stops delivering messages, which from the outside is indistinguishable
// from nobody having written to you.
func sessionNotice(raw any) (wuphfpb.NoticeKind, string, bool) {
	switch evt := raw.(type) {
	case *events.LoggedOut:
		detail := "WhatsApp ended the session"
		if evt.OnConnect {
			detail = fmt.Sprintf("WhatsApp rejected the session on connect (%s)", evt.Reason)
		}
		return wuphfpb.NoticeKind_NOTICE_KIND_LOGGED_OUT, detail, true
	case *events.StreamReplaced:
		// Another client took the session over; this one is done until it is
		// paired again.
		return wuphfpb.NoticeKind_NOTICE_KIND_LOGGED_OUT,
			"another WhatsApp client replaced this session", true
	case *events.ClientOutdated:
		// WhatsApp refuses to serve the client version whatsmeow announces, so
		// the fix is a connector upgrade rather than a re-pair.
		return wuphfpb.NoticeKind_NOTICE_KIND_AUTH_REQUIRED,
			"WhatsApp rejected this client as outdated; upgrade the connector", true
	case *events.TemporaryBan:
		return wuphfpb.NoticeKind_NOTICE_KIND_DEGRADED, evt.String(), true
	case *events.Connected:
		// Paired with the above: a reconnect after trouble tells the owner the
		// bridge is back. The core drops it when nothing was reported wrong.
		return wuphfpb.NoticeKind_NOTICE_KIND_RECOVERED, "", true
	default:
		return wuphfpb.NoticeKind_NOTICE_KIND_UNSPECIFIED, "", false
	}
}
