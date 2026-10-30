//! The client identity the session registers as. WhatsApp does not reliably
//! deliver view-once media to a web companion, so the session announces itself
//! as an Android client, the approach the Go connector takes (Baileys#2201):
//! an Android user-agent platform without web info, and a linked device marked
//! as an Android phone.

use whatsapp_rust::ClientProfile;
use whatsapp_rust::wacore::store::DevicePropsOverride;
use whatsapp_rust::waproto::whatsapp as wa;

use crate::config::ClientMode;

/// The OS label WhatsApp shows for this linked device.
const ANDROID_DEVICE_OS: &str = "Android";
/// The user agent's OS version: whatsmeow's, which the Go connector sends.
const OS_VERSION: &str = "0.1";
/// The user agent's device: whatsmeow's. The Go connector only flips the
/// platform of whatsmeow's web payload, so its user agent still says
/// "Desktop"; this one matches it field for field.
const USER_AGENT_DEVICE: &str = "Desktop";

/// The user agent and device properties for `mode`.
pub fn client_identity(mode: ClientMode) -> (ClientProfile, DevicePropsOverride) {
    let mut profile = match mode {
        ClientMode::Android => ClientProfile::android(OS_VERSION),
        ClientMode::AndroidBusiness => ClientProfile::smb_android(OS_VERSION),
    };
    profile.device = USER_AGENT_DEVICE.to_owned();
    let props = DevicePropsOverride::new()
        .with_os(ANDROID_DEVICE_OS)
        .with_platform_type(wa::device_props::PlatformType::ANDROID_PHONE);
    (profile, props)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wa::client_payload::user_agent::Platform;

    #[test]
    fn each_mode_registers_as_its_android_client() {
        for (mode, platform) in [
            (ClientMode::Android, Platform::ANDROID),
            (ClientMode::AndroidBusiness, Platform::SMB_ANDROID),
        ] {
            let (profile, props) = client_identity(mode);
            assert_eq!(profile.user_agent_platform, platform);
            assert!(!profile.include_web_info);
            assert_eq!(profile.device, "Desktop");
            assert_eq!(profile.os_version, "0.1");
            assert_eq!(
                props.platform_type,
                Some(wa::device_props::PlatformType::ANDROID_PHONE)
            );
            assert_eq!(props.os.as_deref(), Some("Android"));
        }
    }
}
