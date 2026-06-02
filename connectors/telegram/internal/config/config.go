package config

import (
	"errors"
	"fmt"
	"os"
	"strconv"

	"github.com/pelletier/go-toml/v2"
)

// EditMode selects how an inbound edit is mirrored onto Telegram.
type EditMode string

const (
	// EditModeInPlace revises the original mirrored copy in place (default).
	EditModeInPlace EditMode = "edit"
	// EditModeRepost leaves the original copy untouched and posts the revised
	// content as a new message replying to it, marked as an edit.
	EditModeRepost EditMode = "repost"
)

// DeleteMode selects how an inbound delete is mirrored onto Telegram.
type DeleteMode string

const (
	// DeleteModeRevoke deletes the mirrored copy (default).
	DeleteModeRevoke DeleteMode = "delete"
	// DeleteModeTombstone leaves the copy in place and posts a notice replying
	// to it that the original was deleted.
	DeleteModeTombstone DeleteMode = "tombstone"
)

type File struct {
	CoreAddress         string     `toml:"core_address"`
	ConnectorID         string     `toml:"connector_id"`
	SessionFile         string     `toml:"session_file"`
	SupportsThreads     bool       `toml:"supports_threads"`
	SupportsTopicCreate bool       `toml:"supports_topic_create"`
	EditMode            EditMode   `toml:"edit_mode"`
	DeleteMode          DeleteMode `toml:"delete_mode"`
}

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
	cfg.applyDefaults()
	if err := cfg.Validate(); err != nil {
		return File{}, err
	}
	return cfg, nil
}

// applyDefaults fills unset rendering modes with their in-place defaults.
func (c *File) applyDefaults() {
	if c.EditMode == "" {
		c.EditMode = EditModeInPlace
	}
	if c.DeleteMode == "" {
		c.DeleteMode = DeleteModeRevoke
	}
}

func (c File) Validate() error {
	if c.CoreAddress == "" || c.ConnectorID == "" || c.SessionFile == "" {
		return errors.New("core_address, connector_id, and session_file are required")
	}
	if c.SupportsTopicCreate && !c.SupportsThreads {
		return errors.New("supports_topic_create requires supports_threads")
	}
	switch c.EditMode {
	case "", EditModeInPlace, EditModeRepost:
	default:
		return fmt.Errorf("edit_mode must be %q or %q", EditModeInPlace, EditModeRepost)
	}
	switch c.DeleteMode {
	case "", DeleteModeRevoke, DeleteModeTombstone:
	default:
		return fmt.Errorf("delete_mode must be %q or %q", DeleteModeRevoke, DeleteModeTombstone)
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
