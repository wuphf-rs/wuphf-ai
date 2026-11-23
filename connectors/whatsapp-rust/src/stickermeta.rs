//! Sticker-pack metadata, written into a WebP sticker as WhatsApp expects: an
//! EXIF chunk holding a little-endian TIFF block with one private tag (0x5741,
//! "AW") whose value is a JSON document. WhatsApp shows only four keys for a
//! received sticker and has no link field, so the link is folded into the
//! publisher (visible, not clickable). Byte-compatible with the Go connector's
//! `stickerExif` and with `webpmux -set exif`, without the tool: the shared
//! fixtures in `connectors/whatsapp/internal/whatsapp/testdata/` check both.

use std::fmt;

use serde::Serialize;
use tracing::debug;

use crate::config::StickerMetadata;

pub const STICKER_PACK_ID: &str = "ai.wuphf.bridge";

/// `VP8X` flags: the file carries EXIF metadata, and alpha.
const EXIF_FLAG: u8 = 0x08;
const ALPHA_FLAG: u8 = 0x10;

/// The JSON WhatsApp reads, in the key order Go's `encoding/json` writes a
/// map's keys (sorted).
#[derive(Serialize)]
struct Pack<'a> {
    emojis: [&'a str; 1],
    #[serde(rename = "sticker-pack-id")]
    id: &'a str,
    #[serde(rename = "sticker-pack-name")]
    name: &'a str,
    #[serde(rename = "sticker-pack-publisher")]
    publisher: String,
}

/// The EXIF payload (Go `stickerExif`).
pub fn sticker_exif(meta: &StickerMetadata) -> Vec<u8> {
    let publisher = if meta.link.is_empty() {
        meta.author_name.clone()
    } else {
        format!("{} · {}", meta.author_name, meta.link)
            .trim()
            .to_owned()
    };
    let json = serde_json::to_string(&Pack {
        emojis: ["🐶"],
        id: STICKER_PACK_ID,
        name: &meta.pack_name,
        publisher,
    })
    .expect("a struct of strings serializes");
    let json = go_html_escape(&json);
    let mut exif = vec![
        0x49, 0x49, 0x2A, 0x00, 0x08, 0x00, 0x00, 0x00, 0x01, 0x00, 0x41, 0x57, 0x07, 0x00,
    ];
    exif.extend_from_slice(&(json.len() as u32).to_le_bytes());
    exif.extend_from_slice(&[0x16, 0x00, 0x00, 0x00]);
    exif.extend_from_slice(json.as_bytes());
    exif
}

/// The escapes Go's `encoding/json` applies by default. A blind replace is
/// safe: these characters only occur inside the JSON's strings.
fn go_html_escape(json: &str) -> String {
    json.replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029")
}

/// Why a WebP could not be given its EXIF.
#[derive(Debug, PartialEq, Eq)]
pub enum WebpError {
    NotWebp,
    Truncated,
    UnknownLayout(String),
}

impl fmt::Display for WebpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WebpError::NotWebp => f.write_str("not a RIFF/WEBP file"),
            WebpError::Truncated => f.write_str("a chunk runs past the end of the file"),
            WebpError::UnknownLayout(first) => write!(f, "unknown first chunk {first:?}"),
        }
    }
}

impl std::error::Error for WebpError {}

/// One RIFF chunk: its four-character code and payload.
struct Chunk<'a> {
    fourcc: [u8; 4],
    payload: &'a [u8],
}

fn chunks(webp: &[u8]) -> Result<Vec<Chunk<'_>>, WebpError> {
    if webp.len() < 12 || &webp[..4] != b"RIFF" || &webp[8..12] != b"WEBP" {
        return Err(WebpError::NotWebp);
    }
    let mut rest = &webp[12..];
    let mut found = Vec::new();
    while !rest.is_empty() {
        if rest.len() < 8 {
            return Err(WebpError::Truncated);
        }
        let fourcc: [u8; 4] = rest[..4].try_into().unwrap();
        let size = u32::from_le_bytes(rest[4..8].try_into().unwrap()) as usize;
        let padded = size + size % 2;
        if rest.len() < 8 + size {
            return Err(WebpError::Truncated);
        }
        found.push(Chunk {
            fourcc,
            payload: &rest[8..8 + size],
        });
        rest = &rest[(8 + padded).min(rest.len())..];
    }
    Ok(found)
}

fn put_chunk(out: &mut Vec<u8>, fourcc: &[u8; 4], payload: &[u8]) {
    out.extend_from_slice(fourcc);
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(payload);
    if payload.len() % 2 == 1 {
        out.push(0);
    }
}

/// A `VP8X` payload for a simple file's bitstream: its canvas, and the flags.
fn vp8x_for(chunk: &Chunk<'_>) -> Result<[u8; 10], WebpError> {
    let (width, height, alpha) = match &chunk.fourcc {
        b"VP8 " => {
            // A 3-byte frame tag, the 9d 01 2a start code, then the width and
            // height as 14-bit little-endian values.
            let p = chunk.payload;
            if p.len() < 10 || p[3..6] != [0x9d, 0x01, 0x2a] {
                return Err(WebpError::Truncated);
            }
            let width = u16::from_le_bytes([p[6], p[7]]) & 0x3fff;
            let height = u16::from_le_bytes([p[8], p[9]]) & 0x3fff;
            (u32::from(width), u32::from(height), false)
        }
        b"VP8L" => {
            // The 0x2f signature, then width − 1 and height − 1 (14 bits each)
            // and whether alpha is used.
            let p = chunk.payload;
            if p.len() < 5 || p[0] != 0x2f {
                return Err(WebpError::Truncated);
            }
            let bits = u32::from_le_bytes([p[1], p[2], p[3], p[4]]);
            let width = (bits & 0x3fff) + 1;
            let height = ((bits >> 14) & 0x3fff) + 1;
            let alpha = (bits >> 28) & 1 == 1;
            (width, height, alpha)
        }
        other => {
            return Err(WebpError::UnknownLayout(
                String::from_utf8_lossy(other).into(),
            ));
        }
    };
    let mut vp8x = [0u8; 10];
    vp8x[0] = EXIF_FLAG | if alpha { ALPHA_FLAG } else { 0 };
    vp8x[4..7].copy_from_slice(&(width - 1).to_le_bytes()[..3]);
    vp8x[7..10].copy_from_slice(&(height - 1).to_le_bytes()[..3]);
    Ok(vp8x)
}

/// `webp` with its EXIF chunk set to `exif`, as `webpmux -set exif` writes it:
/// an extended file gets the EXIF flag, loses any old `EXIF` chunk and gains
/// the new one before the first `XMP ` chunk, else at the end; a simple file
/// gets a `VP8X` header in front, then its bitstream, then the `EXIF` chunk.
pub fn set_exif(webp: &[u8], exif: &[u8]) -> Result<Vec<u8>, WebpError> {
    let chunks = chunks(webp)?;
    let first = chunks.first().ok_or(WebpError::Truncated)?;
    let mut body = b"WEBP".to_vec();
    if &first.fourcc == b"VP8X" {
        let mut vp8x: [u8; 10] = first.payload.try_into().map_err(|_| WebpError::Truncated)?;
        vp8x[0] |= EXIF_FLAG;
        put_chunk(&mut body, b"VP8X", &vp8x);
        let mut placed = false;
        for chunk in &chunks[1..] {
            match &chunk.fourcc {
                b"EXIF" => continue,
                b"XMP " if !placed => {
                    put_chunk(&mut body, b"EXIF", exif);
                    placed = true;
                }
                _ => {}
            }
            put_chunk(&mut body, &chunk.fourcc, chunk.payload);
        }
        if !placed {
            put_chunk(&mut body, b"EXIF", exif);
        }
    } else {
        put_chunk(&mut body, b"VP8X", &vp8x_for(first)?);
        for chunk in chunks.iter().filter(|chunk| &chunk.fourcc != b"EXIF") {
            put_chunk(&mut body, &chunk.fourcc, chunk.payload);
        }
        put_chunk(&mut body, b"EXIF", exif);
    }
    let mut out = b"RIFF".to_vec();
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    Ok(out)
}

/// Best-effort, like the Go connector's `applyStickerMetadata`: the sticker
/// with its pack metadata, or unchanged when it cannot be written (logged at
/// debug), so the sticker still sends.
pub fn apply_sticker_metadata(webp: Vec<u8>, meta: &StickerMetadata) -> Vec<u8> {
    match set_exif(&webp, &sticker_exif(meta)) {
        Ok(with_metadata) => with_metadata,
        Err(error) => {
            debug!(%error, "sending a sticker without its pack metadata");
            webp
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    macro_rules! fixture {
        ($name:literal) => {
            include_bytes!(concat!("../../whatsapp/internal/whatsapp/testdata/", $name)).as_slice()
        };
    }

    fn defaults() -> StickerMetadata {
        StickerMetadata {
            pack_name: "wuphf".into(),
            author_name: "wuphf-ai".into(),
            link: "https://github.com/wuphf-rs/wuphf-ai".into(),
        }
    }

    fn exif_chunks(webp: &[u8]) -> Vec<Vec<u8>> {
        chunks(webp)
            .unwrap()
            .into_iter()
            .filter(|chunk| &chunk.fourcc == b"EXIF")
            .map(|chunk| chunk.payload.to_vec())
            .collect()
    }

    #[test]
    fn the_exif_matches_go() {
        assert_eq!(sticker_exif(&defaults()), fixture!("sticker-meta.exif"));
    }

    #[test]
    fn the_exif_escapes_like_go() {
        let meta = StickerMetadata {
            pack_name: "Tom & Jerry <3".into(),
            author_name: "wuphf-ai".into(),
            link: String::new(),
        };
        assert_eq!(sticker_exif(&meta), fixture!("sticker-meta-escaped.exif"));
    }

    #[test]
    fn each_layout_matches_webpmux() {
        let exif = sticker_exif(&defaults());
        for (input, output) in [
            (
                fixture!("sticker-lossless-alpha.webp"),
                fixture!("sticker-lossless-alpha.exif.webp"),
            ),
            (
                fixture!("sticker-lossy.webp"),
                fixture!("sticker-lossy.exif.webp"),
            ),
            (
                fixture!("sticker-animated.webp"),
                fixture!("sticker-animated.exif.webp"),
            ),
        ] {
            assert_eq!(set_exif(input, &exif).unwrap(), output);
        }
    }

    #[test]
    fn setting_exif_twice_replaces_it() {
        let other = sticker_exif(&StickerMetadata {
            pack_name: "mine".into(),
            ..defaults()
        });
        let twice = set_exif(fixture!("sticker-lossy.exif.webp"), &other).unwrap();
        assert_eq!(exif_chunks(&twice), [other]);
    }

    #[test]
    fn an_odd_payload_is_padded() {
        let out = set_exif(fixture!("sticker-lossy.webp"), b"abc").unwrap();
        // The file ends with the 3-byte payload and one pad byte, and the RIFF
        // size counts it.
        assert_eq!(&out[out.len() - 4..], b"abc\0");
        let riff = u32::from_le_bytes(out[4..8].try_into().unwrap()) as usize;
        assert_eq!(riff, out.len() - 8);
        assert_eq!(exif_chunks(&out), [b"abc".to_vec()]);
    }

    #[test]
    fn garbage_is_refused_and_left_alone() {
        assert_eq!(set_exif(b"not a webp", b"x"), Err(WebpError::NotWebp));
        let input = b"not a webp".to_vec();
        assert_eq!(apply_sticker_metadata(input.clone(), &defaults()), input);
    }

    #[test]
    fn a_truncated_chunk_is_refused() {
        let lossy = fixture!("sticker-lossy.webp");
        assert_eq!(
            set_exif(&lossy[..lossy.len() - 10], b"x"),
            Err(WebpError::Truncated)
        );
    }
}
