package sessionlock

import (
	"errors"
	"path/filepath"
	"testing"
)

func TestASessionIsLockedOnce(t *testing.T) {
	session := filepath.Join(t.TempDir(), "whatsapp.session.db")
	held, err := Acquire(session)
	if err != nil {
		t.Fatal(err)
	}
	// flock locks belong to the open file, so a second open in this process
	// conflicts as another process's would.
	if _, err := Acquire(session); !errors.Is(err, ErrHeld) {
		t.Fatalf("held twice: %v", err)
	}
	if err := held.Release(); err != nil {
		t.Fatal(err)
	}
	again, err := Acquire(session)
	if err != nil {
		t.Fatalf("released, it is still held: %v", err)
	}
	again.Release()
}
