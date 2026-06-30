package config

import (
	"errors"
	"fmt"
	"os"

	"github.com/pelletier/go-toml/v2"
)

type File struct {
	CoreAddress     string `toml:"core_address"`
	ConnectorID     string `toml:"connector_id"`
	SessionDatabase string `toml:"session_database"`
	// WhatsApp has no topics; these stay false but are surfaced so the core's
	// capability gate is configured from one place.
	SupportsThreads     bool `toml:"supports_threads"`
	SupportsTopicCreate bool `toml:"supports_topic_create"`
	// LogLevel sets log verbosity (trace, debug, info, warn, error). Empty
	// means info; the WUPHF_LOG_LEVEL environment variable overrides it.
	LogLevel string `toml:"log_level"`
	// Stickers carries the pack metadata embedded in stickers bridged to
	// WhatsApp; empty fields fall back to defaults (see StickerMetadata).
	Stickers StickerMetadata `toml:"sticker_metadata"`
}

// StickerMetadata is the sticker-pack information WhatsApp displays for a bridged
// sticker: the pack name, its author, and a link surfaced as the pack's store
// link so a recipient can find the project.
type StickerMetadata struct {
	PackName   string `toml:"pack_name"`
	AuthorName string `toml:"author_name"`
	Link       string `toml:"link"`
}

// Default sticker-pack metadata, used for any field the configuration leaves
// blank so bridged stickers are always attributed.
const (
	defaultStickerPackName = "wuphf"
	defaultStickerAuthor   = "wuphf-ai"
	defaultStickerLink     = "https://github.com/wuphf/wuphf-ai"
)

// StickerMetadata returns the configured sticker-pack metadata with defaults
// filled in for any blank field.
func (c File) StickerMetadata() StickerMetadata {
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
	if c.CoreAddress == "" || c.ConnectorID == "" || c.SessionDatabase == "" {
		return errors.New("core_address, connector_id, and session_database are required")
	}
	if c.SupportsTopicCreate && !c.SupportsThreads {
		return errors.New("supports_topic_create requires supports_threads")
	}
	return nil
}
