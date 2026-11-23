// Package setup is every wuphf Go binary's `setup` subcommand: the binary's
// config file and the env file its options name, edited on botconf's setup
// screen without the binary running, and the check that keeps the binary's
// checked-in example equal to its description.
//
// A binary describes its config file once, in the struct tags of its config
// type (see botconf's describe package: `toml`, `doc`, `title`, `default`,
// `example`, `hint`, `required`, `env`, `secret`, `domain`, `botconf`,
// `jsonschema`).
package setup

import (
	"errors"
	"flag"
	"fmt"
	"os"
	"strings"

	"github.com/propheci/botconf/go/describe"
	"github.com/propheci/botconf/go/tui"
	"github.com/wuphf-rs/wuphf-ai/connectors/shared/chatdir"
)

// Requested reports whether a binary's arguments (os.Args[1:]) ask for setup.
func Requested(args []string) bool {
	return len(args) > 0 && args[0] == "setup"
}

// Run parses setup's flags (the arguments after `setup`: the files' paths)
// and opens the setup screen. screen is the binary's: its description, its
// own check of a candidate config file (its loader, with the env file's
// variables as they would be written), and any suggestions and actions.
func Run(args []string, screen tui.Setup) error {
	flags := flag.NewFlagSet("setup", flag.ExitOnError)
	config := flags.String("config", "config.local.toml", "the config file to edit")
	envFile := flags.String("env-file", ".env.local", "the env file its options' variables are set in")
	if err := flags.Parse(args); err != nil {
		return err
	}
	screen.ConfigPath, screen.EnvPath = *config, *envFile
	return tui.Run(screen)
}

// ChatDomain is the domain of options holding chats (`domain:"wuphf.chat"`),
// written `connector:chat`, as the core's.
const ChatDomain = "wuphf.chat"

// SuggestChats suggests chats (ChatDomain) from the chat directory dir:
// every connector's file there, title first. With none it says how to get
// one.
func SuggestChats(dir string) tui.SuggestFunc {
	return func(query tui.Query) ([]tui.Suggestion, error) {
		if query.Domain != ChatDomain {
			return nil, nil
		}
		if dir == "" {
			return nil, fmt.Errorf("No chat directory: set %s (or $XDG_STATE_HOME or $HOME).", chatdir.DirectoryEnv)
		}
		directories, err := chatdir.ReadAll(dir)
		if err != nil {
			return nil, err
		}
		if len(directories) == 0 {
			return nil, errors.New("No chats known yet: link a connector in its setup " +
				"(wuphf-<platform> setup, then Link account).")
		}
		var found []tui.Suggestion
		for _, directory := range directories {
			for _, chat := range directory.Chats {
				if len(found) < query.Limit && chat.Matches(query.Text) {
					found = append(found, tui.Suggestion{
						Value: chat.Chat,
						Label: fmt.Sprintf("%s (%s, %s)", chat.Title, chat.Kind, chat.Chat),
					})
				}
			}
		}
		return found, nil
	}
}

// Env reads a variable from a candidate's env file, else from the process
// environment, as the binary will once the env file is loaded.
func Env(candidate tui.Candidate, name string) string {
	if value, ok := candidate.Env[name]; ok {
		return value
	}
	return os.Getenv(name)
}

// RequireToken fails when the variable a config option names holds no value,
// as a binary does at startup.
func RequireToken(candidate tui.Candidate, option, name string) error {
	if name == "" {
		return nil
	}
	if strings.TrimSpace(Env(candidate, name)) == "" {
		return fmt.Errorf("%s (%s) is unset or empty: set its value in the env file (e)", name, option)
	}
	return nil
}

// WriteExamples is the environment variable that makes CheckExample rewrite
// the example instead of comparing it.
const WriteExamples = "WUPHF_WRITE_EXAMPLES"

// CheckExample fails when the checked-in example at path is not the
// description's; with WUPHF_WRITE_EXAMPLES set it writes it first. A binary's
// test calls it, so the example never drifts from the config type.
func CheckExample(description *describe.Description, path string) error {
	generated := description.Example()
	if os.Getenv(WriteExamples) != "" {
		if err := os.WriteFile(path, []byte(generated), 0o644); err != nil {
			return err
		}
	}
	checkedIn, err := os.ReadFile(path)
	if err != nil {
		return err
	}
	if string(checkedIn) != generated {
		return errors.New(path + " differs from the config type's description; regenerate it with " +
			WriteExamples + "=1 go test ./...")
	}
	return nil
}
