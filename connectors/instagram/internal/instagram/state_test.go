package instagramconnector

import (
	"path/filepath"
	"testing"
	"time"
)

func TestStateStoreRoundTripAndClear(t *testing.T) {
	t.Parallel()
	store := NewStateStore(filepath.Join(t.TempDir(), "state.json"))

	// Nothing saved yet reads as "start live", not an error.
	if state, err := store.Load(); err != nil || state.SeqID != 0 {
		t.Fatalf("Load() empty = %+v, %v", state, err)
	}
	saved := SyncState{SeqID: 1234, Timestamp: time.UnixMilli(1700000000000).UTC()}
	if err := store.Save(saved); err != nil {
		t.Fatalf("Save() error = %v", err)
	}
	if state, err := store.Load(); err != nil || state.SeqID != saved.SeqID || !state.Timestamp.Equal(saved.Timestamp) {
		t.Fatalf("Load() = %+v, %v; want %+v", state, err, saved)
	}
	if err := store.Clear(); err != nil {
		t.Fatalf("Clear() error = %v", err)
	}
	if state, err := store.Load(); err != nil || state.SeqID != 0 {
		t.Fatalf("Load() after clear = %+v, %v", state, err)
	}
	// Clearing twice is harmless.
	if err := store.Clear(); err != nil {
		t.Fatalf("Clear() again error = %v", err)
	}
}
