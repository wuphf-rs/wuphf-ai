//! What an outbound dispatch becomes on WhatsApp, decided without the network:
//! each attachment's kind and builder options, over-length text as a file,
//! and the quoted parent of a reply. Mirrors the Go connector's
//! `attachmentMessage`, `textExceedsLimit`, `textFileMessage` and
//! `quotedBody` (`connectors/whatsapp/internal/whatsapp/sender.go`).

use std::path::Path;

use whatsapp_rust::media::{AudioOptions, DocumentOptions, ImageOptions, VideoOptions};
use whatsapp_rust::upload::UploadResponse;
use whatsapp_rust::wacore::proto_helpers::MessageBuilderExt;
use whatsapp_rust::waproto::buffa::MessageField;
use whatsapp_rust::waproto::whatsapp as wa;
use wuphf_connector::Failure;
use wuphf_proto::connector as pb;

use crate::wa_client::{OutboundMedia, StickerOptions};

/// The upload request for one attachment: its kind, or a document when
/// `as_file`; `caption` on an image, video or document. For a sticker it is
/// the document the sender falls back to when the sticker cannot be sent as
/// one (named by [`sticker_file_name`], no caption); the sender builds the
/// sticker itself. An unspecified kind can never succeed, so it is permanent.
pub fn outbound_media(
    attachment: &pb::Attachment,
    caption: &str,
    as_file: bool,
) -> Result<OutboundMedia, Failure> {
    use pb::attachment::Kind;
    let mimetype = optional(attachment.mime_type.as_deref());
    let caption = optional(Some(caption));
    let kind = match attachment.kind() {
        _ if as_file => Kind::File,
        kind => kind,
    };
    Ok(match kind {
        Kind::Image => OutboundMedia::Image(ImageOptions {
            caption,
            mimetype,
            ..Default::default()
        }),
        // A WhatsApp GIF is an MP4 with this flag set, so an animated video
        // (a GIF on the source) loops and plays without sound.
        Kind::Video => OutboundMedia::Video(VideoOptions {
            caption,
            mimetype,
            gif_playback: Some(attachment.animated),
            ..Default::default()
        }),
        Kind::Audio => OutboundMedia::Audio(AudioOptions {
            mimetype,
            ..Default::default()
        }),
        Kind::File => OutboundMedia::Document(DocumentOptions {
            caption,
            mimetype,
            file_name: optional(attachment.file_name.as_deref()),
            ..Default::default()
        }),
        Kind::Sticker => OutboundMedia::Document(DocumentOptions {
            mimetype,
            file_name: Some(sticker_file_name(attachment)),
            ..Default::default()
        }),
        Kind::Unspecified => {
            return Err(Failure::permanent(format!(
                "unsupported outbound attachment kind {:?}",
                attachment.kind
            )));
        }
    })
}

/// Whether `text` is longer than `limit` Unicode scalar values; 0 means
/// unbounded.
pub fn text_exceeds_limit(text: &str, limit: u32) -> bool {
    limit != 0 && text.chars().count() > limit as usize
}

/// Over-length text as a plain-text document, so WhatsApp's per-message length
/// cap neither truncates nor rejects it.
pub fn text_file_media() -> OutboundMedia {
    OutboundMedia::Document(DocumentOptions {
        mimetype: Some("text/plain".into()),
        file_name: Some("message.txt".into()),
        ..Default::default()
    })
}

/// The quoted parent for WhatsApp's reply preview: plain text, or a typed stub
/// of its first attachment (an image or video with its caption and mime type,
/// a document with its caption, mime type and file name, audio and a sticker
/// with their mime type), which clients render as a reply to media.
pub fn quoted_body(reply: &pb::ReplyContext) -> wa::Message {
    use pb::attachment::Kind;
    let text = reply.text.clone().unwrap_or_default();
    let Some(attachment) = reply.attachments.first() else {
        return wa::Message::text(text);
    };
    let caption = optional(Some(&text));
    let mimetype = optional(attachment.mime_type.as_deref());
    match attachment.kind() {
        Kind::Image => wa::Message {
            image_message: MessageField::some(wa::message::ImageMessage {
                caption,
                mimetype,
                ..Default::default()
            }),
            ..Default::default()
        },
        Kind::File => wa::Message {
            document_message: MessageField::some(wa::message::DocumentMessage {
                caption,
                mimetype,
                file_name: optional(attachment.file_name.as_deref()),
                ..Default::default()
            }),
            ..Default::default()
        },
        Kind::Video => wa::Message {
            video_message: MessageField::some(wa::message::VideoMessage {
                caption,
                mimetype,
                ..Default::default()
            }),
            ..Default::default()
        },
        Kind::Audio => wa::Message {
            audio_message: MessageField::some(wa::message::AudioMessage {
                mimetype,
                ..Default::default()
            }),
            ..Default::default()
        },
        Kind::Sticker => wa::Message {
            sticker_message: MessageField::some(wa::message::StickerMessage {
                mimetype,
                ..Default::default()
            }),
            ..Default::default()
        },
        Kind::Unspecified => wa::Message::text(text),
    }
}

/// What an upload returned, as the connector's own value: the library's
/// `UploadResponse` cannot be built outside it, so builders the connector
/// writes take this instead.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UploadedMedia {
    pub url: String,
    pub direct_path: String,
    pub media_key: [u8; 32],
    pub file_enc_sha256: [u8; 32],
    pub file_sha256: [u8; 32],
    pub file_length: u64,
    pub media_key_timestamp: i64,
}

impl From<&UploadResponse> for UploadedMedia {
    fn from(upload: &UploadResponse) -> Self {
        Self {
            url: upload.url.clone(),
            direct_path: upload.direct_path.clone(),
            media_key: upload.media_key,
            file_enc_sha256: upload.file_enc_sha256,
            file_sha256: upload.file_sha256,
            file_length: upload.file_length,
            media_key_timestamp: upload.media_key_timestamp,
        }
    }
}

/// A sticker message, filled by hand as the library's builders fill theirs:
/// whatsapp-rust has no sticker builder.
pub fn sticker_message(upload: UploadedMedia, options: StickerOptions) -> wa::Message {
    wa::Message {
        sticker_message: MessageField::some(wa::message::StickerMessage {
            url: Some(upload.url),
            direct_path: Some(upload.direct_path),
            media_key: Some(upload.media_key.to_vec()),
            file_sha256: Some(upload.file_sha256.to_vec()),
            file_enc_sha256: Some(upload.file_enc_sha256.to_vec()),
            file_length: Some(upload.file_length),
            media_key_timestamp: Some(upload.media_key_timestamp),
            mimetype: options.mimetype,
            is_animated: Some(options.is_animated),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// A sticker's fallback document name: its own, else "sticker" and the source
/// extension (`.webm`, `.tgs`, the file name's, else `.bin`).
pub fn sticker_file_name(attachment: &pb::Attachment) -> String {
    if let Some(name) = optional(attachment.file_name.as_deref()) {
        return name;
    }
    let extension = match attachment.mime_type.as_deref() {
        Some("video/webm") => ".webm".to_owned(),
        Some("application/x-tgsticker") => ".tgs".to_owned(),
        _ => attachment
            .file_name
            .as_deref()
            .and_then(|name| Path::new(name).extension())
            .map(|extension| format!(".{}", extension.to_string_lossy()))
            .unwrap_or_else(|| ".bin".to_owned()),
    };
    format!("sticker{extension}")
}

/// An absent or empty string as `None`, as the Go connector's
/// `optionalProtoString`.
pub fn optional(value: Option<&str>) -> Option<String> {
    value.filter(|value| !value.is_empty()).map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn attachment(kind: pb::attachment::Kind) -> pb::Attachment {
        pb::Attachment {
            media_id: "m".into(),
            kind: kind.into(),
            mime_type: Some("x/y".into()),
            file_name: Some("report.pdf".into()),
            ..Default::default()
        }
    }

    #[test]
    fn each_kind_uploads_with_its_options() {
        use pb::attachment::Kind;
        let OutboundMedia::Image(image) =
            outbound_media(&attachment(Kind::Image), "hi", false).unwrap()
        else {
            panic!("an image");
        };
        assert_eq!(
            (image.caption.as_deref(), image.mimetype.as_deref()),
            (Some("hi"), Some("x/y"))
        );

        let mut gif = attachment(Kind::Video);
        gif.animated = true;
        let OutboundMedia::Video(video) = outbound_media(&gif, "", false).unwrap() else {
            panic!("a video");
        };
        assert_eq!((video.caption, video.gif_playback), (None, Some(true)));
        let OutboundMedia::Video(video) =
            outbound_media(&attachment(Kind::Video), "", false).unwrap()
        else {
            panic!("a video");
        };
        assert_eq!(video.gif_playback, Some(false));

        let OutboundMedia::Audio(audio) =
            outbound_media(&attachment(Kind::Audio), "hi", false).unwrap()
        else {
            panic!("audio");
        };
        assert_eq!((audio.mimetype.as_deref(), audio.ptt), (Some("x/y"), None));

        let OutboundMedia::Document(document) =
            outbound_media(&attachment(Kind::File), "the report", false).unwrap()
        else {
            panic!("a document");
        };
        assert_eq!(document.caption.as_deref(), Some("the report"));
        assert_eq!(document.file_name.as_deref(), Some("report.pdf"));
    }

    #[test]
    fn attachments_as_files_forces_documents() {
        use pb::attachment::Kind;
        for kind in [Kind::Image, Kind::Video, Kind::Audio, Kind::File] {
            let OutboundMedia::Document(document) =
                outbound_media(&attachment(kind), "hi", true).unwrap()
            else {
                panic!("{kind:?} as a file is a document");
            };
            assert_eq!(document.file_name.as_deref(), Some("report.pdf"));
            assert_eq!(document.caption.as_deref(), Some("hi"));
        }
    }

    #[test]
    fn the_sticker_message_is_filled_by_hand() {
        let upload = UploadedMedia {
            url: "https://mmg/x".into(),
            direct_path: "/v/x".into(),
            media_key: [1; 32],
            file_enc_sha256: [2; 32],
            file_sha256: [3; 32],
            file_length: 42,
            media_key_timestamp: 1_700_000_000,
        };
        let options = StickerOptions {
            mimetype: Some("image/webp".into()),
            is_animated: true,
        };
        let message = sticker_message(upload, options);
        let sticker = &message.sticker_message;
        assert_eq!(sticker.url.as_deref(), Some("https://mmg/x"));
        assert_eq!(sticker.direct_path.as_deref(), Some("/v/x"));
        assert_eq!(sticker.media_key.as_deref(), Some([1; 32].as_slice()));
        assert_eq!(sticker.file_enc_sha256.as_deref(), Some([2; 32].as_slice()));
        assert_eq!(sticker.file_sha256.as_deref(), Some([3; 32].as_slice()));
        assert_eq!(sticker.file_length, Some(42));
        assert_eq!(sticker.media_key_timestamp, Some(1_700_000_000));
        assert_eq!(sticker.mimetype.as_deref(), Some("image/webp"));
        assert_eq!(sticker.is_animated, Some(true));
    }

    #[test]
    fn a_sticker_falls_back_to_a_document() {
        let sticker = pb::Attachment {
            kind: pb::attachment::Kind::Sticker.into(),
            mime_type: Some("video/webm".into()),
            ..Default::default()
        };
        let OutboundMedia::Document(document) = outbound_media(&sticker, "hi", false).unwrap()
        else {
            panic!("a document");
        };
        assert_eq!(document.file_name.as_deref(), Some("sticker.webm"));
        assert_eq!(document.caption, None);
        let named = pb::Attachment {
            file_name: Some("cat.tgs".into()),
            ..sticker.clone()
        };
        assert_eq!(sticker_file_name(&named), "cat.tgs");
        let unknown = pb::Attachment {
            mime_type: Some("image/webp".into()),
            ..sticker
        };
        assert_eq!(sticker_file_name(&unknown), "sticker.bin");
    }

    #[test]
    fn an_unspecified_kind_is_permanent() {
        let unspecified = pb::Attachment::default();
        let Err(failure) = outbound_media(&unspecified, "", false) else {
            panic!("an unspecified kind is refused");
        };
        assert!(failure.is_permanent());
    }

    #[test]
    fn text_exceeds_limit_counts_characters() {
        assert!(!text_exceeds_limit("anything at all", 0));
        assert!(!text_exceeds_limit("héllo", 5));
        assert!(text_exceeds_limit("héllo!", 5));
        // Six characters, seven bytes.
        assert_eq!("héllo!".len(), 7);
    }

    #[test]
    fn a_media_parent_is_quoted_as_a_typed_stub() {
        use pb::attachment::Kind;
        let reply = |kind: Option<Kind>| pb::ReplyContext {
            platform_message_id: "wa-1".into(),
            text: Some("look".into()),
            attachments: kind.map(attachment).into_iter().collect(),
            ..Default::default()
        };
        let image = quoted_body(&reply(Some(Kind::Image)));
        assert_eq!(image.image_message.caption.as_deref(), Some("look"));
        assert_eq!(image.image_message.mimetype.as_deref(), Some("x/y"));
        let video = quoted_body(&reply(Some(Kind::Video)));
        assert_eq!(video.video_message.caption.as_deref(), Some("look"));
        let document = quoted_body(&reply(Some(Kind::File)));
        assert_eq!(
            document.document_message.file_name.as_deref(),
            Some("report.pdf")
        );
        let audio = quoted_body(&reply(Some(Kind::Audio)));
        assert_eq!(audio.audio_message.mimetype.as_deref(), Some("x/y"));
        let sticker = quoted_body(&reply(Some(Kind::Sticker)));
        assert_eq!(sticker.sticker_message.mimetype.as_deref(), Some("x/y"));
        let text = quoted_body(&reply(None));
        assert_eq!(text.conversation.as_deref(), Some("look"));
    }
}
