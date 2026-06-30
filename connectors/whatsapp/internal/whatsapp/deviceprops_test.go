package whatsappconnector

import (
	"testing"

	"go.mau.fi/whatsmeow/proto/waCompanionReg"
	"go.mau.fi/whatsmeow/proto/waWa6"
	"go.mau.fi/whatsmeow/store"
)

// TestUseAndroidClientIdentity verifies the session registers as an Android
// client: the user-agent platform is ANDROID, the web-only WebInfo is dropped,
// and the linked device is marked an Android phone, so WhatsApp delivers
// view-once media to it.
func TestUseAndroidClientIdentity(t *testing.T) {
	useAndroidClientIdentity()
	if got := store.BaseClientPayload.GetUserAgent().GetPlatform(); got != waWa6.ClientPayload_UserAgent_ANDROID {
		t.Fatalf("UserAgent.Platform = %v, want ANDROID", got)
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
}
