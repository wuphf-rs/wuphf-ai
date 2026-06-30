package whatsappconnector

import (
	"go.mau.fi/whatsmeow/proto/waCompanionReg"
	"go.mau.fi/whatsmeow/store"
	"google.golang.org/protobuf/proto"
)

// androidDeviceOS is the OS label WhatsApp shows for this linked device.
const androidDeviceOS = "Android"

// useAndroidDeviceProps makes the whatsmeow session present as an Android phone
// in WhatsApp's linked-devices list instead of the default "whatsmeow" web
// companion. It changes only the companion metadata WhatsApp displays
// (DeviceProps); the underlying client payload stays a web multidevice
// companion, which is the only protocol whatsmeow implements, so the pairing
// handshake is unchanged.
//
// whatsmeow reads these package-level globals when it builds the registration
// payload, so this must run before the client connects to pair a new session.
// It is a no-op for an already-paired session (the props are sent only at
// registration), but is safe to call every start.
func useAndroidDeviceProps() {
	store.DeviceProps.PlatformType = waCompanionReg.DeviceProps_ANDROID_PHONE.Enum()
	store.DeviceProps.Os = proto.String(androidDeviceOS)
}
