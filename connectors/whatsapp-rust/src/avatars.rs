//! Chat pictures: fetched from WhatsApp and uploaded to the core's media
//! store, so a chat update can name the picture by media id. The core hashes
//! the bytes and posts a picture only when they changed.

use tracing::warn;
use whatsapp_rust::Jid;
use whatsapp_rust::client::Client;
use whatsapp_rust::http::HttpRequest;
use wuphf_connector::MediaClient;
use wuphf_proto::connector as pb;

/// Fetches chat pictures and uploads them to the core.
pub struct Avatars {
    media: MediaClient,
}

impl Avatars {
    pub fn new(media: MediaClient) -> Self {
        Self { media }
    }

    /// The chat's current picture: `id: ""` when it has none (or the owner
    /// may not see it), else its id and the uploaded media. `None` when the
    /// lookup, the download or the upload failed: the update then goes out
    /// without an avatar.
    pub async fn fetch(&self, client: &Client, jid: &Jid) -> Option<pb::ChatAvatar> {
        let picture = match client.contacts().get_profile_picture(jid, false).await {
            Ok(Some(picture)) => picture,
            Ok(None) => return Some(removed()),
            Err(error) => {
                warn!(chat = %jid, %error, "could not look up a WhatsApp chat picture");
                return None;
            }
        };
        let bytes = match client
            .http_client
            .execute(HttpRequest::get(&picture.url))
            .await
        {
            Ok(response) if (200..300).contains(&response.status_code) => response.body,
            Ok(response) => {
                warn!(
                    chat = %jid,
                    status = response.status_code,
                    "could not download a WhatsApp chat picture"
                );
                return None;
            }
            Err(error) => {
                warn!(chat = %jid, %error, "could not download a WhatsApp chat picture");
                return None;
            }
        };
        match self.media.upload(avatar_metadata(bytes.len()), bytes).await {
            Ok(attachment) => Some(pb::ChatAvatar {
                id: picture.id,
                media_id: Some(attachment.media_id),
            }),
            Err(error) => {
                warn!(chat = %jid, %error, "could not upload a WhatsApp chat picture");
                None
            }
        }
    }
}

/// The avatar of a chat without a picture.
pub fn removed() -> pb::ChatAvatar {
    pb::ChatAvatar {
        id: String::new(),
        media_id: None,
    }
}

/// WhatsApp serves chat pictures as JPEG.
fn avatar_metadata(size: usize) -> pb::MediaMetadata {
    pb::MediaMetadata {
        kind: pb::attachment::Kind::Image.into(),
        file_name: Some("avatar.jpg".into()),
        mime_type: Some("image/jpeg".into()),
        size_bytes: Some(size as u64),
        animated: false,
    }
}
