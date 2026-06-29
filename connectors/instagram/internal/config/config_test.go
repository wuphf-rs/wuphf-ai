package config

import "testing"

func TestShouldBackfillDefaultsToEnabled(t *testing.T) {
	t.Parallel()
	enabled := true
	disabled := false
	cases := []struct {
		name string
		cfg  File
		want bool
	}{
		{"unset defaults to enabled", File{}, true},
		{"explicitly enabled", File{BackfillOnStart: &enabled}, true},
		{"explicitly disabled", File{BackfillOnStart: &disabled}, false},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			if got := tc.cfg.ShouldBackfill(); got != tc.want {
				t.Fatalf("ShouldBackfill() = %v, want %v", got, tc.want)
			}
		})
	}
}
