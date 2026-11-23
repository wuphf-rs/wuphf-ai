// Package chatdir is the chat directory: the chats a connector's account can
// reach, fetched by the connector's setup when its account is linked, so the
// setups of the core and misc can offer chats by name with nothing running.
//
// One file per connector, `<connector_id>.chats.json`, in
// $WUPHF_CHAT_DIRECTORY, else $XDG_STATE_HOME/wuphf/chats, else
// ~/.local/state/wuphf/chats. A connector's setup writes it whole. The core
// shares the format (wuphf-model's chat_directory).
package chatdir

import (
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
)

// Version is the format's version; a file of another version is not read.
const Version = 1

// DirectoryEnv names the directory, over the XDG default.
const DirectoryEnv = "WUPHF_CHAT_DIRECTORY"

// Kind is what a chat is.
type Kind string

const (
	Group   Kind = "group"
	Direct  Kind = "direct"
	Channel Kind = "channel"
)

// Directory is one connector's chats.
type Directory struct {
	Version     int    `json:"version"`
	ConnectorID string `json:"connector_id"`
	// Platform is the connector's platform (whatsapp, telegram, instagram).
	Platform string `json:"platform"`
	// FetchedAtMs is when the chats were fetched, in Unix milliseconds.
	FetchedAtMs int64  `json:"fetched_at_ms"`
	Chats       []Chat `json:"chats"`
}

// Chat is a chat as the connector reports it to the core.
type Chat struct {
	// Chat is `connector:chat`, the id rules match: the chat id as the
	// connector normalizes it, `#` and `%` in it written %23 and %25 (see
	// ChatRef).
	Chat  string `json:"chat"`
	Title string `json:"title"`
	Kind  Kind   `json:"kind"`
}

// ChatRef is a chat's `connector:chat` form, as the core reads it.
func ChatRef(connectorID, chatID string) string {
	escape := strings.NewReplacer("%", "%25", "#", "%23")
	return connectorID + ":" + escape.Replace(chatID)
}

// New is a directory of the chats fetched now (fetchedAtMs).
func New(connectorID, platform string, fetchedAtMs int64, chats []Chat) Directory {
	if chats == nil {
		chats = []Chat{}
	}
	return Directory{Version: Version, ConnectorID: connectorID, Platform: platform, FetchedAtMs: fetchedAtMs, Chats: chats}
}

// Matches reports whether the chat matches what a picker's user typed: a part
// of its title (any case) or of its id. Empty text matches every chat.
func (c Chat) Matches(text string) bool {
	text = strings.ToLower(strings.TrimSpace(text))
	return strings.Contains(strings.ToLower(c.Title), text) || strings.Contains(strings.ToLower(c.Chat), text)
}

// Dir is the directory the files are in, or "" with none of
// $WUPHF_CHAT_DIRECTORY, $XDG_STATE_HOME and $HOME set.
func Dir() string { return DirFrom(os.Getenv) }

// DirFrom is Dir with the environment given.
func DirFrom(getenv func(string) string) string {
	if dir := getenv(DirectoryEnv); dir != "" {
		return dir
	}
	if state := getenv("XDG_STATE_HOME"); state != "" {
		return filepath.Join(state, "wuphf", "chats")
	}
	if home := getenv("HOME"); home != "" {
		return filepath.Join(home, ".local", "state", "wuphf", "chats")
	}
	return ""
}

// Path is the file of connectorID's chats in dir.
func Path(dir, connectorID string) string {
	return filepath.Join(dir, connectorID+".chats.json")
}

// Read is connectorID's chats, or nil when it has no file yet.
func Read(dir, connectorID string) (*Directory, error) {
	path := Path(dir, connectorID)
	text, err := os.ReadFile(path)
	if errors.Is(err, os.ErrNotExist) {
		return nil, nil
	}
	if err != nil {
		return nil, err
	}
	// The version first, so a newer file says so rather than failing on a
	// field it changed.
	var versioned struct {
		Version int `json:"version"`
	}
	if err := json.Unmarshal(text, &versioned); err != nil {
		return nil, fmt.Errorf("%s is not a chat directory: %w", path, err)
	}
	if versioned.Version != Version {
		return nil, fmt.Errorf("%s is version %d, not %d: link the connector again", path, versioned.Version, Version)
	}
	var directory Directory
	if err := json.Unmarshal(text, &directory); err != nil {
		return nil, fmt.Errorf("%s is not a chat directory: %w", path, err)
	}
	if directory.ConnectorID != connectorID {
		return nil, fmt.Errorf("%s holds the chats of %s", path, directory.ConnectorID)
	}
	return &directory, nil
}

// ReadAll is every connector's chats in dir, ordered by connector id; none
// when dir does not exist.
func ReadAll(dir string) ([]Directory, error) {
	paths, err := filepath.Glob(filepath.Join(dir, "*.chats.json"))
	if err != nil {
		return nil, err
	}
	var out []Directory
	for _, path := range paths {
		directory, err := Read(dir, strings.TrimSuffix(filepath.Base(path), ".chats.json"))
		if err != nil {
			return nil, err
		}
		if directory != nil {
			out = append(out, *directory)
		}
	}
	return out, nil
}

// Write writes the file whole (a temporary file, renamed over it), creating
// dir; it returns the file's path.
func (d Directory) Write(dir string) (string, error) {
	path := Path(dir, d.ConnectorID)
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return "", err
	}
	text, err := json.MarshalIndent(d, "", "  ")
	if err != nil {
		return "", err
	}
	temporary := path + ".tmp"
	if err := os.WriteFile(temporary, append(text, '\n'), 0o644); err != nil {
		return "", err
	}
	return path, os.Rename(temporary, path)
}
