package config

import (
	"errors"
	"fmt"
	"os"
	"strconv"

	"github.com/pelletier/go-toml/v2"
)

type File struct {
	CoreAddress         string `toml:"core_address"`
	ConnectorID         string `toml:"connector_id"`
	SessionFile         string `toml:"session_file"`
	SupportsThreads     bool   `toml:"supports_threads"`
	SupportsTopicCreate bool   `toml:"supports_topic_create"`
	// LogLevel sets log verbosity (trace, debug, info, warn, error). Empty
	// means info; the WUPHF_LOG_LEVEL environment variable overrides it.
	LogLevel string `toml:"log_level"`
	// Rendering governs how replicated copies are rendered on Telegram. It is
	// this connector's own policy; the core dispatches content only.
	Rendering RenderingOptions `toml:"rendering"`
}

// RenderingOptions are the connector-owned knobs for how mirrored copies are
// rendered on the platform.
type RenderingOptions struct {
	// Send every attachment using the generic file/document method, regardless
	// of its origin kind (image/video/audio/sticker).
	AttachmentsAsFiles bool `toml:"attachments_as_files"`
	// Maximum text length, in characters, to carry inline in one message; text
	// over the limit is sent as a plain-text file instead. Zero is unbounded.
	MaxMessageLength uint32 `toml:"max_message_length"`
	// EditMode picks how a replicated edit is rendered: "in_place" (default)
	// revises the original copy, "repost" leaves it untouched and posts the
	// revision as a reply marked as an edit.
	EditMode string `toml:"edit_mode"`
	// DeleteMode picks how a replicated delete is rendered: "revoke" (default)
	// deletes the copy, "tombstone" leaves it and posts a deletion notice.
	DeleteMode string `toml:"delete_mode"`
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
	if c.CoreAddress == "" || c.ConnectorID == "" || c.SessionFile == "" {
		return errors.New("core_address, connector_id, and session_file are required")
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
