// Package setup is every wuphf Go binary's `setup` subcommand: the binary's
// config file and the env file its options name, edited on botconf's setup
// screen without the binary running, and the check that keeps the binary's
// checked-in example equal to its description.
//
// A binary describes its config file once, in the struct tags of its config
// type (see botconf's describe package: `toml`, `doc`, `title`, `default`,
// `example`, `hint`, `required`, `env`, `secret`, `botconf`, `jsonschema`).
package setup

import (
	"errors"
	"flag"
	"fmt"
	"os"
	"strings"

	"github.com/propheci/botconf/go/describe"
	"github.com/propheci/botconf/go/tui"
)

// Requested reports whether a binary's arguments (os.Args[1:]) ask for setup.
func Requested(args []string) bool {
	return len(args) > 0 && args[0] == "setup"
}

// Run parses setup's flags (the arguments after `setup`) and opens the setup
// screen. validate is the binary's own check of a candidate config file (its
// loader), with the env file's variables as they would be written.
func Run(args []string, description *describe.Description, validate func(tui.Candidate) error) error {
	flags := flag.NewFlagSet("setup", flag.ExitOnError)
	config := flags.String("config", "config.local.toml", "the config file to edit")
	envFile := flags.String("env-file", ".env.local", "the env file its options' variables are set in")
	if err := flags.Parse(args); err != nil {
		return err
	}
	return tui.Run(tui.Setup{
		Description: description,
		ConfigPath:  *config,
		EnvPath:     *envFile,
		Validate:    validate,
	})
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
