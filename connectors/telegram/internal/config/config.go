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
	"github.com/propheci/botconf/go/describe"
)

// File is the connector's config file. Each option is described once, in its
// tags (see the shared setup package); config.example.toml is generated from
// them.
type File struct {
	CoreAddress string `toml:"core_address" required:"true" example:"127.0.0.1:50051" doc:"The core's gRPC address."`
	ConnectorID string `toml:"connector_id" required:"true" example:"telegram-main" doc:"Must match a [[connectors]] entry in the core's configuration."`
	// CoreTokenEnv names the variable; the token itself is read from the
	// environment at startup.
	CoreTokenEnv        string `toml:"core_token_env" required:"true" env:"token" example:"WUPHF_CONNECTOR_TELEGRAM_MAIN_TOKEN" doc:"Environment variable holding this connector's core token; must match the core's [[connectors]].token_env for this connector id."`
	SessionFile         string `toml:"session_file" required:"true" example:"./telegram.session.json" doc:"Where the Telegram session is kept. Reusing it restores the logged-in session."`
	SupportsThreads     bool   `toml:"supports_threads" example:"true" doc:"Telegram supports forum topics, so the core may use this connector as a threaded mirror. Must match the core's [[connectors]] entry."`
	SupportsTopicCreate bool   `toml:"supports_topic_create" example:"true" doc:"The core may provision one topic per source conversation (needs supports_threads). Must match the core's [[connectors]] entry."`
	FunctionsTokenEnv   string `toml:"functions_token_env" env:"token" hint:"WUPHF_INVOKE_TOKEN" doc:"Environment variable holding the core's invoke token, when the core sets [functions] invoke_token_env.\n\nAnimated stickers from other platforms (animated WebP) are converted to Telegram video stickers by the core's media.convert function, served by the providers/mediaconv process. Without it they arrive as static stickers."`
	// BotToken and PanelBotToken are read once and never declared as runtime
	// settings, so they never leave this process.
	BotToken      string       `toml:"bot_token" secret:"true" doc:"The @BotFather token, for a bot session. WUPHF_TELEGRAM_BOT_TOKEN wins over it. Keep this file private when it holds the token.\n\nA bot session needs two things set up on the bot itself, or the bridge runs one-way without any error to show for it: privacy mode off (@BotFather → /setprivacy → Disable) or the bot made an admin, or it only receives commands, replies and mentions; and admin in the mirror group with \"manage topics\", which provisioning and renaming a topic per conversation requires. A bot also cannot open a chat with someone who has not messaged it first, so any rule (or core notify target) pointing at telegram:user:... needs that person to have started the bot."`
	PanelBotToken string       `toml:"panel_bot_token" secret:"true" doc:"A helper bot's token, for a user session's settings panel. WUPHF_TELEGRAM_PANEL_BOT_TOKEN wins over it.\n\nA user session cannot attach buttons to its messages, so it shows the panel through a helper bot: create one with @BotFather, turn on inline mode (/setinline), set its token here, and set [panel] session_file. Without one the panel is sent as text with numbered options. A bot session needs none of this. Owners can also send the panel command to the helper bot in a private chat from their own account, which gets an ordinary button panel: use that when the bridge runs on an account other than yours, and list your own user id in the core's owner_ids, not the bridge account's."`
	Panel         PanelOptions `toml:"panel" doc:"The settings panel, opened with a command (/wuphf by default): the core's settings with buttons. The core only obeys the Telegram user ids listed in its [[connectors]].owner_ids for this connector; for a user session that is the session's own account id. The command is never mirrored, whoever sends it. Read once at startup."`
	// Runtime holds the settings that may change while the connector runs.
	// Embedded, so the file keeps its layout (`log_level`, `[rendering]`).
	Runtime
}

// Description is the config file's description, from File's tags.
func Description() *describe.Description {
	return describe.MustOf("wuphf Telegram connector", fileDescription, File{})
}

const fileDescription = "Runtime settings (log_level, media_convert_timeout_ms, [rendering]) can " +
	"also change while the connector runs: the connector declares them to the core, which keeps any " +
	"change (made from its settings panel or `wuphf-server settings`, under " +
	"`connector_settings.<connector id>`) and pushes it back here. The values in this file are the " +
	"base; unsetting a change returns to them.\n\n" +
	"The Telegram application credentials are read from the environment: WUPHF_TELEGRAM_APP_ID and " +
	"WUPHF_TELEGRAM_APP_HASH, and for a user session WUPHF_TELEGRAM_PHONE and optionally " +
	"WUPHF_TELEGRAM_PASSWORD (authentication codes are prompted on first login).\n\n" +
	"Routing is owned entirely by the core: configure [rules.<id>] entries there, using typed chat " +
	"ids such as telegram:channel:1234567890, telegram:chat:7 or telegram:user:88."

// Runtime holds the settings that can change while the connector runs. The
// connector declares them to the core (the `botconf` tags, see the shared
// settings package), which stores, validates and pushes every change; the
// values in the file are the base. Everything else in File is read once.
//
// Each doc tag's first paragraph is what the settings panel shows.
type Runtime struct {
	LogLevel              string           `toml:"log_level" botconf:"Log level" example:"info" doc:"How much the connector logs (WUPHF_LOG_LEVEL wins)\n\nAny case; anything else is refused. Empty means info." jsonschema:"enum=trace,enum=debug,enum=info,enum=warn,enum=error"`
	MediaConvertTimeoutMs uint32           `toml:"media_convert_timeout_ms" botconf:"Sticker conversion timeout (ms)" doc:"How long converting a sticker may take, waiting for the converter included; 0 means 20000\n\nThis connector delivers in order, so later messages wait behind a conversion."`
	Rendering             RenderingOptions `toml:"rendering" doc:"How replicated copies are rendered on Telegram: this connector's own policy; the core dispatches content only."`
}

// RenderingOptions are the connector-owned knobs for how mirrored copies are
// rendered on the platform.
type RenderingOptions struct {
	AttachmentsAsFiles bool   `toml:"attachments_as_files" botconf:"Attachments as files" doc:"Send every attachment as a file, whatever its kind"`
	MaxMessageLength   uint32 `toml:"max_message_length" botconf:"Longest inline message" hint:"4096" doc:"Longer text is sent as a file; 0 means no limit\n\nCounted in characters; the file is plain text."`
	EditMode           string `toml:"edit_mode" botconf:"Edits" default:"in_place" example:"in_place" doc:"in_place revises the copy; repost replies to it with the new text\n\nrepost leaves the mirrored copy untouched and posts the revision as a reply marked as an edit." jsonschema:"enum=in_place,enum=repost"`
	DeleteMode         string `toml:"delete_mode" botconf:"Deletes" default:"revoke" example:"revoke" doc:"revoke deletes the copy; tombstone replies to it with a notice" jsonschema:"enum=revoke,enum=tombstone"`
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

// DefaultPanelCommand opens the settings panel when panel.command is unset.
const DefaultPanelCommand = "wuphf"

// PanelOptions configure the settings panel. They are read once at startup.
type PanelOptions struct {
	// Empty means DefaultPanelCommand.
	Command     string `toml:"command" default:"wuphf" doc:"The command that opens the panel, without the slash: 1 to 32 lowercase letters, digits or underscores."`
	SessionFile string `toml:"session_file" hint:"./telegram-panel.session.json" doc:"Where the helper bot keeps its session (user sessions with panel_bot_token only)."`
}

// CommandName is the panel command without the slash, defaulted.
func (p PanelOptions) CommandName() string {
	if p.Command == "" {
		return DefaultPanelCommand
	}
	return p.Command
}

// validPanelCommand says whether name can be a Telegram bot command: 1 to 32
// lowercase letters, digits and underscores.
func validPanelCommand(name string) bool {
	if name == "" || len(name) > 32 {
		return false
	}
	for _, r := range name {
		if !(r >= 'a' && r <= 'z' || r >= '0' && r <= '9' || r == '_') {
			return false
		}
	}
	return true
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
	// PanelBotToken is the helper bot's token for user-session panels.
	PanelBotToken string
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
	if c.CoreAddress == "" || c.ConnectorID == "" || c.CoreTokenEnv == "" || c.SessionFile == "" {
		return errors.New("core_address, connector_id, core_token_env, and session_file are required")
	}
	if !validPanelCommand(c.Panel.CommandName()) {
		return fmt.Errorf("panel.command %q must be 1 to 32 lowercase letters, digits or underscores", c.Panel.Command)
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

// LoadCredentials reads the Telegram credentials from the environment. The bot
// token falls back to the file's bot_token when WUPHF_TELEGRAM_BOT_TOKEN is
// unset.
func LoadCredentials(file File) (Credentials, error) {
	appID, err := strconv.Atoi(os.Getenv("WUPHF_TELEGRAM_APP_ID"))
	if err != nil || appID <= 0 {
		return Credentials{}, errors.New("WUPHF_TELEGRAM_APP_ID must be a positive integer")
	}
	credentials := Credentials{
		AppID:    appID,
		AppHash:  os.Getenv("WUPHF_TELEGRAM_APP_HASH"),
		Phone:    os.Getenv("WUPHF_TELEGRAM_PHONE"),
		Password: os.Getenv("WUPHF_TELEGRAM_PASSWORD"),
		BotToken: strings.TrimSpace(os.Getenv("WUPHF_TELEGRAM_BOT_TOKEN")),
	}
	if credentials.BotToken == "" {
		credentials.BotToken = strings.TrimSpace(file.BotToken)
	}
	credentials.PanelBotToken = strings.TrimSpace(os.Getenv("WUPHF_TELEGRAM_PANEL_BOT_TOKEN"))
	if credentials.PanelBotToken == "" {
		credentials.PanelBotToken = strings.TrimSpace(file.PanelBotToken)
	}
	if credentials.AppHash == "" {
		return Credentials{}, errors.New("WUPHF_TELEGRAM_APP_HASH is required")
	}
	if credentials.BotToken == "" && credentials.Phone == "" {
		return Credentials{}, errors.New("set bot_token (or WUPHF_TELEGRAM_BOT_TOKEN) or WUPHF_TELEGRAM_PHONE")
	}
	if credentials.BotToken == "" && credentials.PanelBotToken != "" && file.Panel.SessionFile == "" {
		return Credentials{}, errors.New("panel.session_file is required for the panel bot (panel_bot_token)")
	}
	return credentials, nil
}
