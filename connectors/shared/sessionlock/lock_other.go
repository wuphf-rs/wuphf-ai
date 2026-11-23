//go:build !unix

package sessionlock

import "os"

// lock does nothing where there is no flock: the session is not guarded.
func lock(*os.File) error { return nil }
