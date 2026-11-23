//! A binary out of a download: the download itself, or a file in a `.tar.gz`.

use std::io::Read;

use flate2::read::GzDecoder;

use crate::Error;

/// Whether an asset name is an archive.
pub fn is_archive(name: &str) -> bool {
    name.ends_with(".tar.gz") || name.ends_with(".tgz")
}

/// The binary in a download named `asset`: the download, or the regular file
/// named `binary` (at any depth) in the archive, at most `limit` bytes.
pub fn binary(asset: &str, download: Vec<u8>, binary: &str, limit: u64) -> Result<Vec<u8>, Error> {
    if !is_archive(asset) {
        return Ok(download);
    }
    let invalid = |error: std::io::Error| Error::Verify(format!("{asset}: {error}"));
    let mut archive = tar::Archive::new(GzDecoder::new(download.as_slice()));
    for entry in archive.entries().map_err(invalid)? {
        let entry = entry.map_err(invalid)?;
        let named = entry
            .path()
            .map_err(invalid)?
            .file_name()
            .is_some_and(|name| name == binary);
        if !named || !entry.header().entry_type().is_file() {
            continue;
        }
        let mut contents = Vec::new();
        entry
            .take(limit + 1)
            .read_to_end(&mut contents)
            .map_err(invalid)?;
        if contents.len() as u64 > limit {
            return Err(Error::Verify(format!(
                "{asset}: {binary} is larger than {limit} bytes"
            )));
        }
        return Ok(contents);
    }
    Err(Error::Verify(format!(
        "{asset} holds no file named {binary}"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn archive(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        for (path, contents) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(contents.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder.append_data(&mut header, path, *contents).unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap()
    }

    #[test]
    fn a_plain_download_is_the_binary() {
        assert_eq!(
            binary("wuphf-misc-linux-x86_64", b"ELF".to_vec(), "wuphf-misc", 10).unwrap(),
            b"ELF"
        );
    }

    #[test]
    fn an_archive_gives_the_file_named_as_the_binary() {
        let download = archive(&[("README.md", b"docs"), ("my-misc-1.0/wuphf-misc", b"ELF")]);
        assert_eq!(
            binary("my-misc-1.0.tar.gz", download.clone(), "wuphf-misc", 10).unwrap(),
            b"ELF"
        );
        let missing =
            binary("my-misc-1.0.tgz", download.clone(), "wuphf-telegram", 10).unwrap_err();
        assert!(
            missing.to_string().contains("no file named wuphf-telegram"),
            "{missing}"
        );
        let large = binary("my-misc-1.0.tgz", download, "wuphf-misc", 2).unwrap_err();
        assert!(large.to_string().contains("larger than 2 bytes"), "{large}");
    }
}
