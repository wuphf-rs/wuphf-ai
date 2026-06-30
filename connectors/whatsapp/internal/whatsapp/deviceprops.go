package whatsappconnector

import (
	"go.mau.fi/whatsmeow/proto/waCompanionReg"
	"go.mau.fi/whatsmeow/proto/waWa6"
	"go.mau.fi/whatsmeow/store"
	"google.golang.org/protobuf/proto"
)

// androidDeviceOS is the OS label WhatsApp shows for this linked device.
const androidDeviceOS = "Android"

// useAndroidClientIdentity makes the whatsmeow session register as an Android
// client instead of the default web companion. WhatsApp does not reliably
// deliver view-once media (and some other content) to a web companion, so the
// session announces the Android platform the way the Baileys library does
// (WhiskeySockets/Baileys#2201): set the client payload's user-agent platform to
// ANDROID, drop the web-only WebInfo, and mark the linked device as an Android
// phone. When business is true it registers as the WhatsApp Business (SMB)
// Android client, which an SMB account requires; both are otherwise identical.
//
// This is experimental: it changes the protocol identity WhatsApp sees, not just
// the displayed device label. whatsmeow reads these package-level globals when it
// builds the registration payload, so this must run before the client connects to
// pair a new session. It is a no-op for an already-paired session (the identity
// is sent only at registration), but is safe to call every start.
func useAndroidClientIdentity(business bool) {
	platform := waWa6.ClientPayload_UserAgent_ANDROID
	if business {
		platform = waWa6.ClientPayload_UserAgent_SMB_ANDROID
	}
	store.BaseClientPayload.UserAgent.Platform = platform.Enum()
	store.BaseClientPayload.WebInfo = nil
	store.DeviceProps.PlatformType = waCompanionReg.DeviceProps_ANDROID_PHONE.Enum()
	store.DeviceProps.Os = proto.String(androidDeviceOS)
}
