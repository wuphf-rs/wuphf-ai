package instagramconnector

import (
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"time"
)

// SyncState is the position in Instagram's delta stream the connector has
// bridged up to. Reconnecting from it replays the deltas missed while the
// connector was down, Instagram's counterpart to WhatsApp's offline sync.
type SyncState struct {
	SeqID     int64     `json:"seq_id"`
	Timestamp time.Time `json:"seq_ts"`
}

// StateStore persists SyncState as a small JSON file.
type StateStore struct {
	path string
}

func NewStateStore(path string) StateStore {
	return StateStore{path: path}
}

// Load returns the saved state, or a zero state when none has been saved yet.
func (s StateStore) Load() (SyncState, error) {
	data, err := os.ReadFile(s.path)
	if errors.Is(err, os.ErrNotExist) {
		return SyncState{}, nil
	} else if err != nil {
		return SyncState{}, fmt.Errorf("read Instagram sync state: %w", err)
	}
	var state SyncState
	if err := json.Unmarshal(data, &state); err != nil {
		return SyncState{}, fmt.Errorf("decode Instagram sync state: %w", err)
	}
	return state, nil
}

// Save writes the state atomically, so a crash mid-write leaves the previous
// position rather than a truncated file.
func (s StateStore) Save(state SyncState) error {
	data, err := json.Marshal(state)
	if err != nil {
		return fmt.Errorf("encode Instagram sync state: %w", err)
	}
	tmp, err := os.CreateTemp(filepath.Dir(s.path), filepath.Base(s.path)+".*")
	if err != nil {
		return fmt.Errorf("create Instagram sync state: %w", err)
	}
	defer os.Remove(tmp.Name())
	if _, err := tmp.Write(data); err != nil {
		tmp.Close()
		return fmt.Errorf("write Instagram sync state: %w", err)
	}
	if err := tmp.Close(); err != nil {
		return fmt.Errorf("write Instagram sync state: %w", err)
	}
	if err := os.Rename(tmp.Name(), s.path); err != nil {
		return fmt.Errorf("replace Instagram sync state: %w", err)
	}
	return nil
}

// Clear forgets the saved position, after Instagram declared it too old to
// resume from.
func (s StateStore) Clear() error {
	if err := os.Remove(s.path); err != nil && !errors.Is(err, os.ErrNotExist) {
		return fmt.Errorf("remove Instagram sync state: %w", err)
	}
	return nil
}
