package config

import (
	"errors"
	"fmt"
	"os"
	"strings"
	"time"

	"github.com/pelletier/go-toml/v2"
	"github.com/propheci/botconf/go/describe"
	"github.com/wuphf-rs/wuphf-ai/connectors/shared/logging"
)

// File is the connector's config file. Each option is described once, in its
// tags (see the shared setup package); config.example.toml is generated from
// them.
type File struct {
	CoreAddress string `toml:"core_address" required:"true" example:"127.0.0.1:50051" doc:"The core's gRPC address."`
	ConnectorID string `toml:"connector_id" required:"true" example:"whatsapp-main" doc:"Must match a [[connectors]] entry in the core's configuration."`
	// CoreTokenEnv names the variable; the token itself is read from the
	// environment at startup.
	CoreTokenEnv    string `toml:"core_token_env" required:"true" env:"token" example:"WUPHF_CONNECTOR_WHATSAPP_MAIN_TOKEN" doc:"Environment variable holding this connector's core token; must match the core's [[connectors]].token_env for this connector id."`
	SessionDatabase string `toml:"session_database" required:"true" example:"file:whatsapp.session.db?_foreign_keys=on" doc:"The SQLite database (a URI) holding the WhatsApp device session. Reusing it restores the paired session; after a logout, delete it and restart the connector to pair again."`
	// WhatsApp has no topics; these stay false but are surfaced so the core's
	// capability gate is configured from one place.
	SupportsThreads            bool   `toml:"supports_threads" doc:"WhatsApp has no threads: leave false."`
	SupportsTopicCreate        bool   `toml:"supports_topic_create" doc:"WhatsApp has no threads: leave false."`
	SupportsDisappearingTimers bool   `toml:"supports_disappearing_timers" example:"true" doc:"Stamp each chat's disappearing-messages timer on what the connector sends, so contacts do not see \"sent from an older version of WhatsApp, will not disappear\"; look a group's timer up when the core does not know it, and warn the owner when a DM's timer is unknown.\n\nPart of the registration, so it must match supports_disappearing_timers in the core's [[connectors]] entry for this connector: when only one side sets it, the core refuses the connector."`
	ClientMode                 string `toml:"client_mode" default:"android" example:"android_business" jsonschema:"enum=android,enum=android_business" doc:"Which WhatsApp client the session registers as. Both pair as an Android client, so WhatsApp delivers view-once media, which it withholds from a web companion; they differ only in the account type they match: android for a personal account, android_business for a WhatsApp Business (SMB) account.\n\nExperimental, and applied only at the next fresh pairing: after a change, unlink the device on the phone, move the session database aside and pair again."`
	LoginMethod                string `toml:"login_method" hint:"qr" jsonschema:"enum=qr,enum=pair_code" doc:"How the session is linked, at the first start and again by itself after WhatsApp logs it out: qr (scan a QR code in WhatsApp → Linked devices → Link a device) or pair_code (type an 8-character code after \"Link with phone number instead\"; needs phone_number).\n\nEither way the code is printed in the connector's log and sent to the owner through the core's notice chats (the QR code as a picture): the first at once, later ones at growing gaps until it is linked. Unset: pair_code when phone_number is set, else qr."`
	PhoneNumber                string `toml:"phone_number" hint:"15551234567" doc:"The account's number, country code first, digits only (no +): the number a pair code is issued for. Required by login_method = \"pair_code\"."`
	FunctionsTokenEnv          string `toml:"functions_token_env" env:"token" hint:"WUPHF_INVOKE_TOKEN" doc:"Environment variable holding the core's invoke token, when the core sets [functions] invoke_token_env.\n\nAnimated stickers from other platforms (Telegram video and Lottie stickers) are converted to WebP by the core's media.convert provider (providers/mediaconv); without one they arrive as files."`
	// Runtime holds the settings that may change while the connector runs.
	// Embedded, so the file keeps its layout (`log_level`, `[rendering]`).
	Runtime
}

// Description is the config file's description, from File's tags.
func Description() *describe.Description {
	return describe.MustOf("wuphf WhatsApp connector", fileDescription, File{})
}

const fileDescription = "Runtime settings (log_level, media_convert_timeout_ms, [sticker_metadata], " +
	"[rendering]) can also change while the connector runs: the connector declares them to the core, " +
	"which keeps any change (made from its settings panel or `wuphf-server settings`, under " +
	"`connector_settings.<connector id>`) and pushes it back here. The values in this file are the " +
	"base; unsetting a change returns to them.\n\n" +
	"Routing is owned entirely by the core: configure [rules.<id>] entries there, using WhatsApp JIDs " +
	"such as 12345@s.whatsapp.net or 12345@g.us as chat ids."

// Runtime holds the settings that can change while the connector runs. The
// connector declares them to the core (the `botconf` tags, see the shared
// settings package), which stores, validates and pushes every change; the
// values in the file are the base. Everything else in File is read once.
//
// Each doc tag's first paragraph is what the settings panel shows; the Rust
// connector declares the same words (the shared fixture test checks both).
type Runtime struct {
	LogLevel              string           `toml:"log_level" botconf:"Log level" example:"info" doc:"How much the connector logs (WUPHF_LOG_LEVEL wins)\n\nAny case; anything else is refused. Empty means info." jsonschema:"enum=trace,enum=debug,enum=info,enum=warn,enum=error"`
	MediaConvertTimeoutMs uint32           `toml:"media_convert_timeout_ms" botconf:"Sticker conversion timeout (ms)" doc:"How long converting a sticker may take, waiting for the converter included; 0 means 20000\n\nThis connector delivers in order, so later messages wait behind a conversion."`
	Stickers              StickerMetadata  `toml:"sticker_metadata" doc:"Sticker-pack metadata embedded in stickers bridged to WhatsApp, shown in WhatsApp's sticker info. A field left blank falls back to the default shown. WhatsApp has no link field for received stickers, so the link is appended to the publisher text (visible, not clickable)."`
	Rendering             RenderingOptions `toml:"rendering" doc:"How replicated copies are rendered on WhatsApp: this connector's own policy; the core dispatches content only."`
}

// RenderingOptions are the connector-owned knobs for how mirrored copies are
// rendered on the platform.
type RenderingOptions struct {
	AttachmentsAsFiles bool   `toml:"attachments_as_files" botconf:"Attachments as files" doc:"Send every attachment as a file, whatever its kind"`
	MaxMessageLength   uint32 `toml:"max_message_length" botconf:"Longest inline message" hint:"4096" doc:"Longer text is sent as a file; 0 means no limit\n\nCounted in characters; the file is plain text."`
}

// StickerMetadata is the sticker-pack information WhatsApp displays for a bridged
// sticker: the pack name, its author, and a link surfaced as the pack's store
// link so a recipient can find the project.
type StickerMetadata struct {
	PackName   string `toml:"pack_name" botconf:"Sticker pack name" hint:"wuphf" doc:"The pack name WhatsApp shows for bridged stickers"`
	AuthorName string `toml:"author_name" botconf:"Sticker author" hint:"wuphf-ai" doc:"The author WhatsApp shows for bridged stickers"`
	Link       string `toml:"link" botconf:"Sticker link" hint:"https://github.com/wuphf/wuphf-ai" doc:"A link folded into the sticker's publisher"`
}

// Default sticker-pack metadata, used for any field the configuration leaves
// blank so bridged stickers are always attributed.
const (
	defaultStickerPackName = "wuphf"
	defaultStickerAuthor   = "wuphf-ai"
	defaultStickerLink     = "https://github.com/wuphf/wuphf-ai"
)

// defaultMediaConvertTimeout bounds a sticker conversion when the
// configuration sets no timeout.
const defaultMediaConvertTimeout = 20 * time.Second

// MediaConvertTimeout bounds one sticker conversion through media.convert,
// including the time spent waiting for a converter to be available.
func (c Runtime) MediaConvertTimeout() time.Duration {
	if c.MediaConvertTimeoutMs == 0 {
		return defaultMediaConvertTimeout
	}
	return time.Duration(c.MediaConvertTimeoutMs) * time.Millisecond
}

// StickerMetadata returns the configured sticker-pack metadata with defaults
// filled in for any blank field.
func (c Runtime) StickerMetadata() StickerMetadata {
	meta := c.Stickers
	if meta.PackName == "" {
		meta.PackName = defaultStickerPackName
	}
	if meta.AuthorName == "" {
		meta.AuthorName = defaultStickerAuthor
	}
	if meta.Link == "" {
		meta.Link = defaultStickerLink
	}
	return meta
}

// WithDefaults fills in what an empty setting stands for, so the values
// declared to the core are explicit.
func (c Runtime) WithDefaults() Runtime {
	c.LogLevel = strings.ToLower(c.LogLevel)
	if c.LogLevel == "" {
		c.LogLevel = "info"
	}
	c.Stickers = c.StickerMetadata()
	return c
}

// WhatsApp client identities the session can register as. Both pair as an
// Android client (so WhatsApp delivers view-once media, unlike a web companion);
// they differ only in whether the account is a personal or a WhatsApp Business
// (SMB) one, which the registration must match.
const (
	ClientModeAndroid         = "android"
	ClientModeAndroidBusiness = "android_business"
)

// ResolvedClientMode returns the configured client mode, defaulting to the
// personal Android identity when unset.
func (c File) ResolvedClientMode() string {
	if c.ClientMode == "" {
		return ClientModeAndroid
	}
	return c.ClientMode
}

// How the session is linked to the account.
const (
	// LoginMethodQR scans a QR code in WhatsApp → Linked devices → Link a device.
	LoginMethodQR = "qr"
	// LoginMethodPairCode types an 8-character code into WhatsApp after "Link
	// with phone number instead"; it needs PhoneNumber.
	LoginMethodPairCode = "pair_code"
)

// ResolvedLoginMethod returns the configured login method, defaulting to a
// pair code when a phone number is set and to a QR code otherwise.
func (c File) ResolvedLoginMethod() string {
	if c.LoginMethod != "" {
		return c.LoginMethod
	}
	if c.PhoneNumber != "" {
		return LoginMethodPairCode
	}
	return LoginMethodQR
}

// SessionFile is the session database's file: session_database without its
// `file:` scheme (and empty authority) and its URI parameters.
func (c File) SessionFile() string {
	path, _, _ := strings.Cut(c.SessionDatabase, "?")
	if rest, ok := strings.CutPrefix(path, "file:"); ok {
		path = rest
		if rest, ok := strings.CutPrefix(path, "//"); ok {
			path = rest
		}
	}
	return path
}

func Load(path string) (File, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return File{}, fmt.Errorf("read configuration: %w", err)
	}
	return Parse(data)
}

// Parse reads and checks a configuration's text.
func Parse(data []byte) (File, error) {
	var cfg File
	if err := toml.Unmarshal(data, &cfg); err != nil {
		return File{}, fmt.Errorf("decode configuration: %w", err)
	}
	if err := cfg.Validate(); err != nil {
		return File{}, err
	}
	return cfg, nil
}

func (c File) Validate() error {
	if err := logging.CheckLevel(c.LogLevel); err != nil {
		return err
	}
	if c.CoreAddress == "" || c.ConnectorID == "" || c.CoreTokenEnv == "" || c.SessionDatabase == "" {
		return errors.New("core_address, connector_id, core_token_env, and session_database are required")
	}
	if c.SupportsTopicCreate && !c.SupportsThreads {
		return errors.New("supports_topic_create requires supports_threads")
	}
	switch c.ClientMode {
	case "", ClientModeAndroid, ClientModeAndroidBusiness:
	default:
		return fmt.Errorf("client_mode must be %q or %q", ClientModeAndroid, ClientModeAndroidBusiness)
	}
	switch c.LoginMethod {
	case "", LoginMethodQR, LoginMethodPairCode:
	default:
		return fmt.Errorf("login_method must be %q or %q", LoginMethodQR, LoginMethodPairCode)
	}
	if c.ResolvedLoginMethod() == LoginMethodPairCode && c.PhoneNumber == "" {
		return fmt.Errorf("login_method = %q needs phone_number", LoginMethodPairCode)
	}
	return nil
}
