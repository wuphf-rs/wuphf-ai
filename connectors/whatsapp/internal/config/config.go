package config

import (
	"errors"
	"fmt"
	"os"
	"strings"
	"time"

	"github.com/akshettrj/wuphf/connectors/shared/logging"
	"github.com/pelletier/go-toml/v2"
)

type File struct {
	CoreAddress string `toml:"core_address"`
	ConnectorID string `toml:"connector_id"`
	// CoreTokenEnv names the environment variable holding this connector's core
	// token; it must match the core's [[connectors]].token_env for this
	// connector id. Required.
	CoreTokenEnv    string `toml:"core_token_env"`
	SessionDatabase string `toml:"session_database"`
	// WhatsApp has no topics; these stay false but are surfaced so the core's
	// capability gate is configured from one place.
	SupportsThreads     bool `toml:"supports_threads"`
	SupportsTopicCreate bool `toml:"supports_topic_create"`
	// ClientMode selects which WhatsApp client identity the session registers
	// as. Empty defaults to ClientModeAndroid; see ClientMode for the choices.
	ClientMode string `toml:"client_mode"`
	// FunctionsTokenEnv names the environment variable holding the core's
	// invoke token, when the core requires one.
	FunctionsTokenEnv string `toml:"functions_token_env"`
	// Runtime holds the settings that may change while the connector runs.
	// Embedded, so the file keeps its layout (`log_level`, `[rendering]`).
	Runtime
}

// Runtime holds the settings that can change while the connector runs. The
// connector declares them to the core (the `botconf` tags, see the shared
// settings package), which stores, validates and pushes every change; the
// values in the file are the base. Everything else in File is read once.
type Runtime struct {
	// LogLevel sets log verbosity (trace, debug, info, warn, error). Empty
	// means info; the WUPHF_LOG_LEVEL environment variable overrides it.
	LogLevel string `toml:"log_level" botconf:"Log level" doc:"How much the connector logs (WUPHF_LOG_LEVEL wins)" jsonschema:"enum=trace,enum=debug,enum=info,enum=warn,enum=error"`
	// MediaConvertTimeoutMs bounds a sticker conversion, including waiting for
	// the converter; deliveries on this connector wait behind it. 0 = 20000.
	MediaConvertTimeoutMs uint32 `toml:"media_convert_timeout_ms" botconf:"Sticker conversion timeout (ms)" doc:"How long converting a sticker may take, waiting for the converter included; 0 means 20000"`
	// Stickers carries the pack metadata embedded in stickers bridged to
	// WhatsApp; empty fields fall back to defaults (see StickerMetadata).
	Stickers StickerMetadata `toml:"sticker_metadata"`
	// Rendering governs how replicated copies are rendered on WhatsApp. It is
	// this connector's own policy; the core dispatches content only.
	Rendering RenderingOptions `toml:"rendering"`
}

// RenderingOptions are the connector-owned knobs for how mirrored copies are
// rendered on the platform.
type RenderingOptions struct {
	// Send every attachment using the generic file/document method, regardless
	// of its origin kind (image/video/audio/sticker).
	AttachmentsAsFiles bool `toml:"attachments_as_files" botconf:"Attachments as files" doc:"Send every attachment as a file, whatever its kind"`
	// Maximum text length, in characters, to carry inline in one message; text
	// over the limit is sent as a plain-text file instead. Zero is unbounded.
	MaxMessageLength uint32 `toml:"max_message_length" botconf:"Longest inline message" doc:"Longer text is sent as a file; 0 means no limit"`
}

// StickerMetadata is the sticker-pack information WhatsApp displays for a bridged
// sticker: the pack name, its author, and a link surfaced as the pack's store
// link so a recipient can find the project.
type StickerMetadata struct {
	PackName   string `toml:"pack_name" botconf:"Sticker pack name" doc:"The pack name WhatsApp shows for bridged stickers"`
	AuthorName string `toml:"author_name" botconf:"Sticker author" doc:"The author WhatsApp shows for bridged stickers"`
	Link       string `toml:"link" botconf:"Sticker link" doc:"A link folded into the sticker's publisher"`
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

func Load(path string) (File, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return File{}, fmt.Errorf("read configuration: %w", err)
	}
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
	return nil
}
