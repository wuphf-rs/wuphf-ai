package config

import (
	"errors"
	"fmt"
	"os"
	"strconv"
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
	CoreTokenEnv        string `toml:"core_token_env"`
	SessionFile         string `toml:"session_file"`
	SupportsThreads     bool   `toml:"supports_threads"`
	SupportsTopicCreate bool   `toml:"supports_topic_create"`
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
	// Rendering governs how replicated copies are rendered on Telegram. It is
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
	// EditMode picks how a replicated edit is rendered: "in_place" (default)
	// revises the original copy, "repost" leaves it untouched and posts the
	// revision as a reply marked as an edit.
	EditMode string `toml:"edit_mode" botconf:"Edits" doc:"in_place revises the copy; repost replies to it with the new text" jsonschema:"enum=in_place,enum=repost"`
	// DeleteMode picks how a replicated delete is rendered: "revoke" (default)
	// deletes the copy, "tombstone" leaves it and posts a deletion notice.
	DeleteMode string `toml:"delete_mode" botconf:"Deletes" doc:"revoke deletes the copy; tombstone replies to it with a notice" jsonschema:"enum=revoke,enum=tombstone"`
}

// defaultMediaConvertTimeout bounds a sticker conversion when the
// configuration sets no timeout.
const defaultMediaConvertTimeout = 20 * time.Second

// WithDefaults fills in what an empty setting stands for, so the values
// declared to the core are explicit (and pass its schema).
func (c Runtime) WithDefaults() Runtime {
	c.LogLevel = strings.ToLower(c.LogLevel)
	if c.LogLevel == "" {
		c.LogLevel = "info"
	}
	if c.Rendering.EditMode == "" {
		c.Rendering.EditMode = EditModeInPlace
	}
	if c.Rendering.DeleteMode == "" {
		c.Rendering.DeleteMode = DeleteModeRevoke
	}
	return c
}

// MediaConvertTimeout bounds one sticker conversion through media.convert,
// including the time spent waiting for a converter to be available.
func (c Runtime) MediaConvertTimeout() time.Duration {
	if c.MediaConvertTimeoutMs == 0 {
		return defaultMediaConvertTimeout
	}
	return time.Duration(c.MediaConvertTimeoutMs) * time.Millisecond
}

// Rendering modes a configuration may name.
const (
	EditModeInPlace     = "in_place"
	EditModeRepost      = "repost"
	DeleteModeRevoke    = "revoke"
	DeleteModeTombstone = "tombstone"
)

type Credentials struct {
	AppID    int
	AppHash  string
	Phone    string
	Password string
	BotToken string
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
	if c.CoreAddress == "" || c.ConnectorID == "" || c.CoreTokenEnv == "" || c.SessionFile == "" {
		return errors.New("core_address, connector_id, core_token_env, and session_file are required")
	}
	if c.SupportsTopicCreate && !c.SupportsThreads {
		return errors.New("supports_topic_create requires supports_threads")
	}
	switch c.Rendering.EditMode {
	case "", EditModeInPlace, EditModeRepost:
	default:
		return fmt.Errorf("rendering.edit_mode must be %q or %q", EditModeInPlace, EditModeRepost)
	}
	switch c.Rendering.DeleteMode {
	case "", DeleteModeRevoke, DeleteModeTombstone:
	default:
		return fmt.Errorf("rendering.delete_mode must be %q or %q", DeleteModeRevoke, DeleteModeTombstone)
	}
	return nil
}

func LoadCredentials() (Credentials, error) {
	appID, err := strconv.Atoi(os.Getenv("WUPHF_TELEGRAM_APP_ID"))
	if err != nil || appID <= 0 {
		return Credentials{}, errors.New("WUPHF_TELEGRAM_APP_ID must be a positive integer")
	}
	credentials := Credentials{
		AppID:    appID,
		AppHash:  os.Getenv("WUPHF_TELEGRAM_APP_HASH"),
		Phone:    os.Getenv("WUPHF_TELEGRAM_PHONE"),
		Password: os.Getenv("WUPHF_TELEGRAM_PASSWORD"),
		BotToken: os.Getenv("WUPHF_TELEGRAM_BOT_TOKEN"),
	}
	if credentials.AppHash == "" {
		return Credentials{}, errors.New("WUPHF_TELEGRAM_APP_HASH is required")
	}
	if credentials.BotToken == "" && credentials.Phone == "" {
		return Credentials{}, errors.New("set WUPHF_TELEGRAM_BOT_TOKEN or WUPHF_TELEGRAM_PHONE")
	}
	return credentials, nil
}
