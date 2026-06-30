package whatsappconnector

import (
	"testing"

	"go.mau.fi/whatsmeow/proto/waCompanionReg"
	"go.mau.fi/whatsmeow/store"
)

// TestUseAndroidDeviceProps verifies the session is registered as an Android
// phone so WhatsApp shows it as an Android device in linked devices.
func TestUseAndroidDeviceProps(t *testing.T) {
	useAndroidDeviceProps()
	if got := store.DeviceProps.GetPlatformType(); got != waCompanionReg.DeviceProps_ANDROID_PHONE {
		t.Fatalf("PlatformType = %v, want ANDROID_PHONE", got)
	}
	if got := store.DeviceProps.GetOs(); got != androidDeviceOS {
		t.Fatalf("Os = %q, want %q", got, androidDeviceOS)
	}
}
