//! Source builds: the repository checked out at the release's tag, its build
//! command run, the binaries it put in `$OUT` read back.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use crate::Error;
use crate::forge::Release;
use crate::source::Source;

/// Build `binaries` from `source` at `release`'s tag, in a checkout kept under
/// `state_dir` (reused, so later builds are incremental), returning each
/// binary's contents. The build's output goes to this process's.
pub async fn build(
    source: &Source,
    release: &Release,
    binaries: &[String],
    state_dir: &Path,
) -> Result<Vec<(String, Vec<u8>)>, Error> {
    let command = source
        .build
        .as_deref()
        .ok_or_else(|| Error::Source(format!("{} has no build command", source.name)))?;
    let checkout = state_dir.join("src").join(dir_name(&source.name));
    let out = state_dir.join("build").join(dir_name(&source.name));

    if !checkout.join(".git").is_dir() {
        std::fs::create_dir_all(&checkout)
            .map_err(|error| Error::Source(format!("create {}: {error}", checkout.display())))?;
        git(
            &checkout,
            &["clone", "--quiet", "--", &source.repository.web(), "."],
        )
        .await?;
    }
    git(
        &checkout,
        &["fetch", "--quiet", "--force", "--tags", "origin"],
    )
    .await?;
    let tag = format!("refs/tags/{}", release.tag);
    git(
        &checkout,
        &["checkout", "--quiet", "--force", "--detach", &tag],
    )
    .await?;

    match std::fs::remove_dir_all(&out) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            return Err(Error::Source(format!("clear {}: {error}", out.display())));
        }
        _ => {}
    }
    std::fs::create_dir_all(&out)
        .map_err(|error| Error::Source(format!("create {}: {error}", out.display())))?;
    let status = tokio::process::Command::new("sh")
        .args(["-c", command])
        .current_dir(&checkout)
        .env("OUT", &out)
        .stdin(Stdio::null())
        .status()
        .await
        .map_err(|error| Error::Source(format!("run the build: {error}")))?;
    if !status.success() {
        return Err(Error::Source(format!(
            "{}'s build at {} failed ({status})",
            source.name, release.tag
        )));
    }
    binaries
        .iter()
        .map(|binary| {
            let built = out.join(binary);
            std::fs::read(&built)
                .map(|contents| (binary.clone(), contents))
                .map_err(|error| {
                    Error::Source(format!("the build made no {}: {error}", built.display()))
                })
        })
        .collect()
}

async fn git(dir: &Path, args: &[&str]) -> Result<(), Error> {
    let output = tokio::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|error| Error::Source(format!("run git: {error}")))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(Error::Source(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }
}

/// A source name as a directory name.
fn dir_name(name: &str) -> PathBuf {
    let safe: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    PathBuf::from(safe)
}
