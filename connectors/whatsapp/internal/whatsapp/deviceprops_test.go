package whatsappconnector

import (
	"testing"

	"go.mau.fi/whatsmeow/proto/waCompanionReg"
	"go.mau.fi/whatsmeow/proto/waWa6"
	"go.mau.fi/whatsmeow/store"
)

// TestUseAndroidClientIdentity verifies each mode registers as the right Android
// client: personal uses the ANDROID platform, business uses SMB_ANDROID. Both
// drop the web-only WebInfo and mark the device an Android phone, so WhatsApp
// delivers view-once media to the companion.
func TestUseAndroidClientIdentity(t *testing.T) {
	cases := []struct {
		name     string
		business bool
		want     waWa6.ClientPayload_UserAgent_Platform
	}{
		{"personal", false, waWa6.ClientPayload_UserAgent_ANDROID},
		{"business", true, waWa6.ClientPayload_UserAgent_SMB_ANDROID},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			useAndroidClientIdentity(tc.business)
			if got := store.BaseClientPayload.GetUserAgent().GetPlatform(); got != tc.want {
				t.Fatalf("UserAgent.Platform = %v, want %v", got, tc.want)
			}
			if store.BaseClientPayload.GetWebInfo() != nil {
				t.Fatal("WebInfo should be dropped for an Android client")
			}
			if got := store.DeviceProps.GetPlatformType(); got != waCompanionReg.DeviceProps_ANDROID_PHONE {
				t.Fatalf("PlatformType = %v, want ANDROID_PHONE", got)
			}
			if got := store.DeviceProps.GetOs(); got != androidDeviceOS {
				t.Fatalf("Os = %q, want %q", got, androidDeviceOS)
			}
		})
	}
}
