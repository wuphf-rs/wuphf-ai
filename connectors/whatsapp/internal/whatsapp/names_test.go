package whatsappconnector

import (
	"testing"

	"go.mau.fi/whatsmeow/types"
)

func TestFormatPersonName(t *testing.T) {
	t.Parallel()

	jid := types.NewJID("15550001111", types.DefaultUserServer)
	cases := []struct {
		name     string
		contact  types.ContactInfo
		pushHint string
		want     string
	}{
		{"saved contact uses address-book name", types.ContactInfo{FullName: "John Doe", PushName: "JD"}, "ignored", "John Doe"},
		{"business is tagged not numbered", types.ContactInfo{BusinessName: "Acme"}, "Acme Sales", "Acme (Business)"},
		{"unsaved with push hint shows id in brackets", types.ContactInfo{}, "Johnny", "Johnny (15550001111)"},
		{"unsaved falls back to stored push name", types.ContactInfo{PushName: "Stored"}, "", "Stored (15550001111)"},
		{"unknown person is just the id", types.ContactInfo{}, "", "15550001111"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			if got := formatPersonName(jid, tc.contact, tc.pushHint); got != tc.want {
				t.Fatalf("formatPersonName() = %q, want %q", got, tc.want)
			}
		})
	}
}

func TestTagged(t *testing.T) {
	t.Parallel()
	if got := tagged("G", "Family"); got != "[G] Family" {
		t.Fatalf("tagged() = %q, want [G] Family", got)
	}
	if got := tagged("P", ""); got != "" {
		t.Fatalf("tagged() = %q, want empty for an empty title", got)
	}
}
