//! Inbound media: what a WhatsApp message's attachment is (described like the
//! Go connector's `inboundMedia`), fetched from WhatsApp and stored in the
//! core's media store so the create event can name it.

use std::fmt;
use std::future::Future;

use whatsapp_rust::client::Client;
use whatsapp_rust::download::Downloadable;
use whatsapp_rust::waproto::whatsapp as wa;
use wuphf_connector::{MediaClient, SessionError};
use wuphf_proto::connector as pb;

/// The attachment a base message carries, and where to download it from.
pub struct InboundMedia<'a> {
    pub metadata: pb::MediaMetadata,
    pub source: &'a dyn Downloadable,
}

/// The media a base message carries, checked in the Go connector's order:
/// image, document, video, audio, sticker. `size_bytes` is the message's
/// `file_length` (0 when absent); empty mime types and file names are absent;
/// only a document carries a file name; `animated` is a video's GIF playback
/// flag or a sticker's animated flag, the only signals that tell a GIF from a
/// video and an animated sticker from a still one.
pub fn inbound_media(base: &wa::Message) -> Option<InboundMedia<'_>> {
    use pb::attachment::Kind;
    let described = |kind: Kind, mime: &Option<String>, length: Option<u64>| pb::MediaMetadata {
        kind: kind.into(),
        file_name: None,
        mime_type: present(mime),
        size_bytes: Some(length.unwrap_or(0)),
        animated: false,
    };
    if let Some(image) = base.image_message.as_option() {
        return Some(InboundMedia {
            metadata: described(Kind::Image, &image.mimetype, image.file_length),
            source: image,
        });
    }
    if let Some(document) = base.document_message.as_option() {
        return Some(InboundMedia {
            metadata: pb::MediaMetadata {
                file_name: present(&document.file_name),
                ..described(Kind::File, &document.mimetype, document.file_length)
            },
            source: document,
        });
    }
    if let Some(video) = base.video_message.as_option() {
        return Some(InboundMedia {
            metadata: pb::MediaMetadata {
                animated: video.gif_playback.unwrap_or(false),
                ..described(Kind::Video, &video.mimetype, video.file_length)
            },
            source: video,
        });
    }
    if let Some(audio) = base.audio_message.as_option() {
        return Some(InboundMedia {
            metadata: described(Kind::Audio, &audio.mimetype, audio.file_length),
            source: audio,
        });
    }
    if let Some(sticker) = base.sticker_message.as_option() {
        return Some(InboundMedia {
            metadata: pb::MediaMetadata {
                animated: sticker.is_animated.unwrap_or(false),
                ..described(Kind::Sticker, &sticker.mimetype, sticker.file_length)
            },
            source: sticker,
        });
    }
    None
}

fn present(value: &Option<String>) -> Option<String> {
    value
        .as_deref()
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

/// Downloads and decrypts WhatsApp media (the Go connector's `Downloader`).
pub trait MediaFetch: Send + Sync {
    fn download(
        &self,
        source: &dyn Downloadable,
    ) -> impl Future<Output = anyhow::Result<Vec<u8>>> + Send;
}

impl MediaFetch for Client {
    fn download(
        &self,
        source: &dyn Downloadable,
    ) -> impl Future<Output = anyhow::Result<Vec<u8>>> + Send {
        Client::download(self, source)
    }
}

/// Stores bytes in the core's media store (the Go connector's
/// `MediaBroker.Upload`).
pub trait MediaSink: Send + Sync {
    fn upload(
        &self,
        metadata: pb::MediaMetadata,
        bytes: Vec<u8>,
    ) -> impl Future<Output = Result<pb::Attachment, SessionError>> + Send;
}

impl MediaSink for MediaClient {
    fn upload(
        &self,
        metadata: pb::MediaMetadata,
        bytes: Vec<u8>,
    ) -> impl Future<Output = Result<pb::Attachment, SessionError>> + Send {
        MediaClient::upload(self, metadata, bytes)
    }
}

/// Why a message's media did not reach the core.
#[derive(Debug)]
pub enum InboundMediaError {
    /// WhatsApp's media could not be downloaded or decrypted.
    Download(anyhow::Error),
    /// The core did not store it.
    Upload(SessionError),
}

impl fmt::Display for InboundMediaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Download(error) => write!(f, "download from WhatsApp: {error:#}"),
            Self::Upload(error) => write!(f, "store in the core: {error}"),
        }
    }
}

/// The message's attachment, downloaded from WhatsApp and stored in the core;
/// `Ok(None)` when the message carries no media.
pub async fn fetch(
    fetch: &impl MediaFetch,
    sink: &impl MediaSink,
    base: &wa::Message,
) -> Result<Option<pb::Attachment>, InboundMediaError> {
    let Some(media) = inbound_media(base) else {
        return Ok(None);
    };
    let bytes = fetch
        .download(media.source)
        .await
        .map_err(InboundMediaError::Download)?;
    sink.upload(media.metadata, bytes)
        .await
        .map(Some)
        .map_err(InboundMediaError::Upload)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use whatsapp_rust::waproto::buffa::MessageField;

    use super::*;
    use whatsapp_rust::wacore::proto_helpers::MessageBuilderExt;

    fn image(mime: &str, length: Option<u64>) -> wa::Message {
        wa::Message {
            image_message: MessageField::some(wa::message::ImageMessage {
                mimetype: Some(mime.into()),
                file_length: length,
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn metadata(message: &wa::Message) -> pb::MediaMetadata {
        inbound_media(message).expect("media").metadata
    }

    #[test]
    fn each_media_kind_is_described_like_go() {
        use pb::attachment::Kind;
        let expected =
            |kind: Kind, mime: &str, file_name: Option<&str>, animated: bool| pb::MediaMetadata {
                kind: kind.into(),
                file_name: file_name.map(str::to_owned),
                mime_type: Some(mime.into()),
                size_bytes: Some(10),
                animated,
            };
        let video = |gif: Option<bool>| wa::Message {
            video_message: MessageField::some(wa::message::VideoMessage {
                mimetype: Some("video/mp4".into()),
                file_length: Some(10),
                gif_playback: gif,
                ..Default::default()
            }),
            ..Default::default()
        };
        let sticker = |animated: Option<bool>| wa::Message {
            sticker_message: MessageField::some(wa::message::StickerMessage {
                mimetype: Some("image/webp".into()),
                file_length: Some(10),
                is_animated: animated,
                ..Default::default()
            }),
            ..Default::default()
        };
        let document = wa::Message {
            document_message: MessageField::some(wa::message::DocumentMessage {
                mimetype: Some("application/pdf".into()),
                file_name: Some("a.pdf".into()),
                file_length: Some(10),
                ..Default::default()
            }),
            ..Default::default()
        };
        let audio = wa::Message {
            audio_message: MessageField::some(wa::message::AudioMessage {
                mimetype: Some("audio/ogg".into()),
                file_length: Some(10),
                ..Default::default()
            }),
            ..Default::default()
        };
        let cases = [
            (
                image("image/jpeg", Some(10)),
                expected(Kind::Image, "image/jpeg", None, false),
            ),
            (
                document,
                expected(Kind::File, "application/pdf", Some("a.pdf"), false),
            ),
            (video(None), expected(Kind::Video, "video/mp4", None, false)),
            (
                video(Some(true)),
                expected(Kind::Video, "video/mp4", None, true),
            ),
            (audio, expected(Kind::Audio, "audio/ogg", None, false)),
            (
                sticker(None),
                expected(Kind::Sticker, "image/webp", None, false),
            ),
            (
                sticker(Some(true)),
                expected(Kind::Sticker, "image/webp", None, true),
            ),
        ];
        for (message, want) in cases {
            assert_eq!(metadata(&message), want);
        }
    }

    #[test]
    fn empty_fields_are_absent() {
        let described = metadata(&image("", None));
        assert_eq!(described.mime_type, None);
        assert_eq!(described.size_bytes, Some(0));
        assert_eq!(described.file_name, None);
    }

    #[test]
    fn text_has_no_media() {
        assert!(inbound_media(&wa::Message::text("hi")).is_none());
    }

    struct FakeFetch(Option<&'static [u8]>);

    impl MediaFetch for FakeFetch {
        async fn download(&self, _source: &dyn Downloadable) -> anyhow::Result<Vec<u8>> {
            self.0
                .map(<[u8]>::to_vec)
                .ok_or_else(|| anyhow::anyhow!("media expired"))
        }
    }

    #[derive(Default)]
    struct FakeSink {
        calls: Mutex<Vec<(pb::MediaMetadata, Vec<u8>)>>,
    }

    impl MediaSink for FakeSink {
        async fn upload(
            &self,
            metadata: pb::MediaMetadata,
            bytes: Vec<u8>,
        ) -> Result<pb::Attachment, SessionError> {
            self.calls.lock().unwrap().push((metadata.clone(), bytes));
            Ok(pb::Attachment {
                media_id: "m-1".into(),
                kind: metadata.kind,
                ..Default::default()
            })
        }
    }

    #[tokio::test]
    async fn fetch_uploads_what_it_downloads() {
        let sink = FakeSink::default();
        let message = image("image/jpeg", Some(4));
        let attachment = fetch(&FakeFetch(Some(b"jpeg")), &sink, &message)
            .await
            .expect("fetched")
            .expect("an attachment");
        assert_eq!(attachment.media_id, "m-1");
        let calls = sink.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, metadata(&message));
        assert_eq!(calls[0].1, b"jpeg");
    }

    #[tokio::test]
    async fn a_failed_download_uploads_nothing() {
        let sink = FakeSink::default();
        let result = fetch(&FakeFetch(None), &sink, &image("image/jpeg", Some(4))).await;
        assert!(matches!(result, Err(InboundMediaError::Download(_))));
        assert!(sink.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_message_without_media_fetches_nothing() {
        let sink = FakeSink::default();
        let result = fetch(&FakeFetch(None), &sink, &wa::Message::text("hi")).await;
        assert!(matches!(result, Ok(None)));
        assert!(sink.calls.lock().unwrap().is_empty());
    }
}
