use std::io::Cursor;

use super::*;

struct Key {
    pair: minisign::KeyPair,
}

impl Key {
    fn new() -> Self {
        Self {
            pair: minisign::KeyPair::generate_unencrypted_keypair().unwrap(),
        }
    }

    fn file(&self) -> String {
        self.pair.pk.to_box().unwrap().to_string()
    }

    fn sign(&self, data: &[u8], comment: &str) -> String {
        minisign::sign(
            Some(&self.pair.pk),
            &self.pair.sk,
            Cursor::new(data),
            Some(comment),
            None,
        )
        .unwrap()
        .to_string()
    }
}

#[test]
fn checksums_are_read_as_sha256sum_writes_them() {
    let sum = "a".repeat(64);
    let text = format!(
        "{sum}  wuphf-server-linux-x86_64\n{}  *binary-mode\nnot a line\nshort  x\n",
        "B".repeat(64)
    );
    let checksums = parse_checksums(&text);
    assert_eq!(checksums.len(), 2);
    assert_eq!(checksums["wuphf-server-linux-x86_64"], sum);
    assert_eq!(checksums["binary-mode"], "b".repeat(64));
}

#[test]
fn a_download_must_match_its_sum() {
    let checksums = parse_checksums(&format!("{}  file\n", sha256_hex(b"right")));
    assert!(check_sum(&checksums, "file", b"right").is_ok());
    assert!(
        check_sum(&checksums, "file", b"wrong")
            .unwrap_err()
            .to_string()
            .contains("SHA-256")
    );
    assert!(
        check_sum(&checksums, "other", b"right")
            .unwrap_err()
            .to_string()
            .contains("not in")
    );
}

#[test]
fn a_signature_verifies_with_the_key_file_or_its_key_line() {
    let key = Key::new();
    let signature = key.sign(b"sums", "wuphf v1.0.0");
    assert!(check_signature(b"sums", &signature, &key.file(), Some("wuphf v1.0.0")).is_ok());
    let key_line = key.file().lines().nth(1).unwrap().to_owned();
    assert!(check_signature(b"sums", &signature, &key_line, None).is_ok());
}

#[test]
fn a_wrong_signature_or_comment_is_refused() {
    let key = Key::new();
    let signature = key.sign(b"sums", "wuphf v1.0.0");
    let tampered = check_signature(b"tampered", &signature, &key.file(), None).unwrap_err();
    assert!(
        tampered.to_string().contains("does not verify"),
        "{tampered}"
    );
    let other = check_signature(b"sums", &signature, &Key::new().file(), None).unwrap_err();
    assert!(other.to_string().contains("does not verify"), "{other}");
    let replayed =
        check_signature(b"sums", &signature, &key.file(), Some("wuphf v2.0.0")).unwrap_err();
    assert!(
        replayed.to_string().contains("not \"wuphf v2.0.0\""),
        "{replayed}"
    );
    let unset = check_signature(b"sums", &signature, "", None).unwrap_err();
    assert!(unset.to_string().contains("no public key"), "{unset}");
}
