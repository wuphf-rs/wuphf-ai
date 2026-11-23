// Package sessionlock is the lock a connector holds on its session while it
// runs, so its setup's actions (linking the account, fetching its chats) never
// use the session at the same time, and two connectors never share one.
//
// The lock is an advisory lock on `<session>.lock`, released when the
// process exits however it exits; the file itself is left.
package sessionlock

import (
	"errors"
	"fmt"
	"os"
)

// ErrHeld is the lock being held by another process.
var ErrHeld = errors.New("the session is in use")

// Lock is a held session lock.
type Lock struct {
	file *os.File
}

// Path is the lock file of session.
func Path(session string) string { return session + ".lock" }

// Acquire takes session's lock without waiting; ErrHeld (wrapped, naming the
// session) when another process holds it.
func Acquire(session string) (*Lock, error) {
	file, err := os.OpenFile(Path(session), os.O_RDWR|os.O_CREATE, 0o600)
	if err != nil {
		return nil, fmt.Errorf("open the session lock: %w", err)
	}
	if err := lock(file); err != nil {
		file.Close()
		if errors.Is(err, ErrHeld) {
			return nil, fmt.Errorf("%s: %w", session, ErrHeld)
		}
		return nil, fmt.Errorf("lock %s: %w", Path(session), err)
	}
	return &Lock{file: file}, nil
}

// Release gives the lock up.
func (l *Lock) Release() error {
	return l.file.Close()
}
