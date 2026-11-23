// Package version is a binary's release version, which every wuphf binary
// prints for `--version` (and `wuphf-server update` reads to tell what is
// installed).
package version

import (
	"fmt"
	"io"
	"os"
	"slices"
)

// Version is the release, stamped by scripts/build-release.sh:
//
//	-ldflags "-X github.com/wuphf-rs/wuphf-ai/connectors/shared/version.Version=X.Y.Z"
//
// A build without the stamp (`go run`, a plain `go build`) is "dev".
var Version = "dev"

// Requested reports whether the arguments ask for the version: a first
// argument of `version`, `--version` or `-version`.
func Requested(args []string) bool {
	return len(args) > 0 && slices.Contains([]string{"version", "--version", "-version"}, args[0])
}

// Print writes "<name> <version>" to standard output.
func Print(name string) {
	write(os.Stdout, name)
}

func write(out io.Writer, name string) {
	fmt.Fprintf(out, "%s %s\n", name, Version)
}
