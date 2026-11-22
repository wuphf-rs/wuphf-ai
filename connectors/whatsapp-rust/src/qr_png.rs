//! A pairing QR code as a PNG, for the owner to scan from a link-required
//! notice. The `qrcode` crate's image renderer would pull in an image crate;
//! the picture is two colours, so a 1-bit greyscale PNG is written here over
//! `flate2`, which the dependency tree already has.

use std::io::Write as _;

use flate2::Compression;
use flate2::write::ZlibEncoder;
use qrcode::{Color, QrCode};

/// Pixels per QR module: a phone camera reads it comfortably off a screen.
const SCALE: usize = 8;
/// Light modules around the code, as the QR specification asks.
const QUIET_ZONE: usize = 4;

/// `payload` (the library's pairing string) as a PNG of its QR code.
pub fn qr_png(payload: &str) -> anyhow::Result<Vec<u8>> {
    let code = QrCode::new(payload.as_bytes())?;
    let modules = code.width();
    let colors = code.to_colors();
    let side = (modules + 2 * QUIET_ZONE) * SCALE;
    let dark = |x: usize, y: usize| {
        let (mx, my) = (x / SCALE, y / SCALE);
        (QUIET_ZONE..QUIET_ZONE + modules).contains(&mx)
            && (QUIET_ZONE..QUIET_ZONE + modules).contains(&my)
            && colors[(my - QUIET_ZONE) * modules + (mx - QUIET_ZONE)] == Color::Dark
    };
    Ok(png_1bit(side, side, dark)?)
}

/// A 1-bit greyscale PNG of `width` × `height` pixels, black where `dark`.
fn png_1bit(
    width: usize,
    height: usize,
    dark: impl Fn(usize, usize) -> bool,
) -> std::io::Result<Vec<u8>> {
    // Each row: filter type 0 (none), then the pixels packed eight to a byte,
    // most significant bit first; 1 is white in greyscale.
    let row_bytes = width.div_ceil(8);
    let mut raw = Vec::with_capacity(height * (row_bytes + 1));
    for y in 0..height {
        raw.push(0);
        for byte in 0..row_bytes {
            let mut bits = 0u8;
            for bit in 0..8 {
                let x = byte * 8 + bit;
                if x >= width || !dark(x, y) {
                    bits |= 0x80 >> bit;
                }
            }
            raw.push(bits);
        }
    }
    let mut zlib = ZlibEncoder::new(Vec::new(), Compression::default());
    zlib.write_all(&raw)?;
    let idat = zlib.finish()?;

    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&(width as u32).to_be_bytes());
    ihdr.extend_from_slice(&(height as u32).to_be_bytes());
    // Bit depth 1, colour type 0 (greyscale), deflate, adaptive filtering,
    // no interlace.
    ihdr.extend_from_slice(&[1, 0, 0, 0, 0]);

    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    chunk(&mut png, b"IHDR", &ihdr);
    chunk(&mut png, b"IDAT", &idat);
    chunk(&mut png, b"IEND", &[]);
    Ok(png)
}

/// Append one chunk: length, type, data, and the CRC of type and data.
fn chunk(png: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    png.extend_from_slice(&(data.len() as u32).to_be_bytes());
    png.extend_from_slice(kind);
    png.extend_from_slice(data);
    let crc = crc32(kind.iter().chain(data));
    png.extend_from_slice(&crc.to_be_bytes());
}

/// The CRC-32 (ISO-HDLC, as PNG uses it) of `bytes`.
fn crc32<'a>(bytes: impl IntoIterator<Item = &'a u8>) -> u32 {
    let mut crc = !0u32;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use std::io::Read as _;

    use flate2::read::ZlibDecoder;

    use super::*;

    /// The chunks of `png` after its signature: (type, data).
    fn chunks(png: &[u8]) -> Vec<([u8; 4], Vec<u8>)> {
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        let mut rest = &png[8..];
        let mut found = Vec::new();
        while !rest.is_empty() {
            let length = u32::from_be_bytes(rest[..4].try_into().unwrap()) as usize;
            let kind: [u8; 4] = rest[4..8].try_into().unwrap();
            let data = rest[8..8 + length].to_vec();
            let crc = u32::from_be_bytes(rest[8 + length..12 + length].try_into().unwrap());
            assert_eq!(crc, crc32(kind.iter().chain(&data)), "chunk CRC");
            found.push((kind, data));
            rest = &rest[12 + length..];
        }
        found
    }

    #[test]
    fn the_crc_matches_the_png_reference() {
        // Every PNG ends with this IEND chunk CRC.
        assert_eq!(crc32(b"IEND"), 0xAE42_6082);
    }

    #[test]
    fn a_qr_code_is_a_well_formed_1_bit_png() {
        let png = qr_png("2@abc,def,ghi,jkl").unwrap();
        let chunks = chunks(&png);
        let kinds: Vec<_> = chunks.iter().map(|(kind, _)| kind).collect();
        assert_eq!(kinds, [b"IHDR", b"IDAT", b"IEND"]);
        let ihdr = &chunks[0].1;
        let width = u32::from_be_bytes(ihdr[..4].try_into().unwrap()) as usize;
        let height = u32::from_be_bytes(ihdr[4..8].try_into().unwrap()) as usize;
        assert_eq!(width, height);
        assert_eq!(width % SCALE, 0);
        assert_eq!(&ihdr[8..], [1, 0, 0, 0, 0]);

        let mut raw = Vec::new();
        ZlibDecoder::new(&chunks[1].1[..])
            .read_to_end(&mut raw)
            .unwrap();
        let row_bytes = width.div_ceil(8);
        assert_eq!(raw.len(), height * (row_bytes + 1));
        let pixel = |x: usize, y: usize| {
            let byte = raw[y * (row_bytes + 1) + 1 + x / 8];
            byte & (0x80 >> (x % 8)) == 0
        };
        // The quiet zone is white; a finder pattern's corner is black.
        assert!(!pixel(0, 0));
        let corner = QUIET_ZONE * SCALE;
        assert!(pixel(corner, corner));
    }
}
