package whatsappconnector

import (
	"context"
	"errors"
	"fmt"
	"os"
	"time"

	coreclient "github.com/akshettrj/wuphf/connectors/shared/coreclient"
	"github.com/akshettrj/wuphf/connectors/shared/functions"
	"github.com/akshettrj/wuphf/connectors/shared/logging"
	"github.com/akshettrj/wuphf/connectors/shared/settings"
	"github.com/akshettrj/wuphf/connectors/whatsapp/internal/config"
	wuphfpb "github.com/akshettrj/wuphf/proto/go/connector/v1"
	_ "github.com/mattn/go-sqlite3"
	"github.com/mdp/qrterminal/v3"
	"github.com/rs/zerolog"
	"go.mau.fi/whatsmeow"
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

func (c Connector) Run(ctx context.Context) error {
	dbLog := waLog.Zerolog(c.logger.With().Str("component", "whatsmeow-db").Logger())
	container, err := sqlstore.New(ctx, "sqlite3", c.cfg.SessionDatabase, dbLog)
	if err != nil {
		return fmt.Errorf("open WhatsApp session database: %w", err)
	}
	defer container.Close()
	// Register as an Android client (experimental) so WhatsApp delivers
	// view-once media to this companion; must be set before pairing, which
	// connect() may trigger below. A WhatsApp Business account pairs as the SMB
	// Android client instead.
	mode := c.cfg.ResolvedClientMode()
	c.logger.Warn().Str("client_mode", mode).Msg("registering WhatsApp session as an Android client (experimental)")
	useAndroidClientIdentity(mode == config.ClientModeAndroidBusiness)
	device, err := container.GetFirstDevice(ctx)
	if err != nil {
		return fmt.Errorf("load WhatsApp device: %w", err)
	}
	client := whatsmeow.NewClient(device, waLog.Zerolog(c.logger.With().Str("component", "whatsmeow").Logger()))
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
	identity := newIdentityResolver(client)
	addresses := newAddressBook(identity, whatsmeowAccount{client: client})
	normalizer := NewNormalizer(inputs, NewMediaCodec(media, client), newChatNamer(client), identity, client, addresses)
	eventErrors := make(chan error, 1)
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
		default:
			// Session-health events do not fail the connector: whatsmeow stops
			// delivering messages but the core session stays up, which is what
			// carries the notice out through the other connectors. Staying
			// alive also means a recovery can be reported on the same session.
			if kind, detail, ok := sessionNotice(raw); ok {
				c.logger.Warn().Str("detail", detail).Msg("WhatsApp session health changed")
				coreclient.SendNotice(ctx, inputs, kind, detail)
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
	if err := connect(ctx, client); err != nil {
		return err
	}
	defer client.Disconnect()
	if client.Store.ID == nil {
		return errors.New("WhatsApp session has no account JID after pairing")
	}
	ownJID := *client.Store.ID
	c.logger.Info().Str("jid", ownJID.String()).Msg("WhatsApp session connected")
	sender := NewSender(client, media, converter, addresses, runtime.Live)
	if c.cfg.SupportsDisappearingTimers {
		sender = sender.WithTimers(newTimerSource(client, inputs, ctx))
	}
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
		}, c.logger).Run(ctx, sender, inputs)
	}()
	select {
	case <-ctx.Done():
		return ctx.Err()
	case err := <-eventErrors:
		return fmt.Errorf("normalize WhatsApp event: %w", err)
	case err := <-runResult:
		return err
	}
}

func connect(ctx context.Context, client *whatsmeow.Client) error {
	if client.Store.ID != nil {
		if err := client.Connect(); err != nil {
			return fmt.Errorf("connect WhatsApp session: %w", err)
		}
		return nil
	}
	codes, err := client.GetQRChannel(ctx)
	if err != nil {
		return fmt.Errorf("open WhatsApp QR pairing: %w", err)
	}
	if err := client.Connect(); err != nil {
		return fmt.Errorf("connect WhatsApp for pairing: %w", err)
	}
	for code := range codes {
		switch code.Event {
		case "code":
			fmt.Fprintln(os.Stderr, "Scan WhatsApp QR code:")
			qrterminal.GenerateHalfBlock(code.Code, qrterminal.L, os.Stderr)
		case "success":
			return nil
		default:
			return fmt.Errorf("WhatsApp QR pairing failed: %s", code.Event)
		}
	}
	return errors.New("WhatsApp QR pairing stopped before success")
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
