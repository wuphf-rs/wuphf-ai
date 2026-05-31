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
