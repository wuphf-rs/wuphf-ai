//! Checking downloads: SHA-256 sums from a checksums file (`sha256sum`'s
//! format) and its minisign signature.

use std::collections::HashMap;

use minisign_verify::{PublicKey, Signature};
use sha2::{Digest, Sha256};

use crate::Error;

/// A download's SHA-256, in lower-case hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// A checksums file: each file name's sum. Lines are `<hex>  <name>` (or
/// `<hex> *<name>`, binary mode); anything else is skipped.
pub fn parse_checksums(text: &str) -> HashMap<String, String> {
    text.lines()
        .filter_map(|line| {
            let (sum, name) = line.split_once(char::is_whitespace)?;
            let name = name.trim_start();
            let name = name.strip_prefix('*').unwrap_or(name).trim_end();
            let valid = sum.len() == 64 && sum.bytes().all(|byte| byte.is_ascii_hexdigit());
            (valid && !name.is_empty()).then(|| (name.to_owned(), sum.to_ascii_lowercase()))
        })
        .collect()
}

/// Whether `bytes` is the file the checksums list as `name`.
pub fn check_sum(
    checksums: &HashMap<String, String>,
    name: &str,
    bytes: &[u8],
) -> Result<(), Error> {
    let expected = checksums
        .get(name)
        .ok_or_else(|| Error::Verify(format!("{name} is not in the release's checksums")))?;
    let actual = sha256_hex(bytes);
    if &actual != expected {
        return Err(Error::Verify(format!(
            "{name}'s SHA-256 is {actual}, the checksums say {expected}"
        )));
    }
    Ok(())
}

/// Whether `signature` (a `.minisig` file) is `public_key`'s on `data`, and,
/// when `comment` is set, its trusted comment says exactly that.
///
/// `public_key` is a minisign `.pub` file or its key line alone.
pub fn check_signature(
    data: &[u8],
    signature: &str,
    public_key: &str,
    comment: Option<&str>,
) -> Result<(), Error> {
    let public_key = public_key.trim();
    if public_key.is_empty() {
        return Err(Error::Verify(
            "no public key to check the signature with (wuphf's release key is unset in this build: set updates.wuphf.public_key)".into(),
        ));
    }
    let key = if public_key.contains('\n') {
        PublicKey::decode(public_key)
    } else {
        PublicKey::from_base64(public_key)
    }
    .map_err(|error| Error::Verify(format!("the public key: {error}")))?;
    let signature = Signature::decode(signature)
        .map_err(|error| Error::Verify(format!("the signature: {error}")))?;
    key.verify(data, &signature, false).map_err(|error| {
        Error::Verify(format!("the checksums' signature does not verify: {error}"))
    })?;
    if let Some(comment) = comment
        && signature.trusted_comment() != comment
    {
        return Err(Error::Verify(format!(
            "the signature is for {:?}, not {comment:?}",
            signature.trusted_comment()
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
