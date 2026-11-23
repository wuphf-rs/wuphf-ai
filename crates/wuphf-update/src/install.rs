//! Installing binaries: each replaced atomically (a rename over the old file,
//! so a running process keeps its own and nothing ever sees half a file), the
//! ones an update replaced kept for `--rollback`, and one update at a time.

use std::fs::{self, File};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use crate::Error;

/// Installs into one directory, keeping what it replaces under a state
/// directory.
#[derive(Debug, Clone)]
pub struct Installer {
    install_dir: PathBuf,
    state_dir: PathBuf,
}

/// Held while an update runs: a second one refuses to start.
#[derive(Debug)]
pub struct Lock {
    _file: File,
}

impl Installer {
    pub fn new(install_dir: PathBuf, state_dir: PathBuf) -> Self {
        Self {
            install_dir,
            state_dir,
        }
    }

    pub fn install_dir(&self) -> &Path {
        &self.install_dir
    }

    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    /// Where `binary` is installed.
    pub fn path(&self, binary: &str) -> PathBuf {
        self.install_dir.join(binary)
    }

    /// The last update's replaced binaries.
    fn previous_dir(&self) -> PathBuf {
        self.state_dir.join("previous")
    }

    /// Take the update lock.
    pub fn lock(&self) -> Result<Lock, Error> {
        let failed = |error: std::io::Error| {
            Error::Install(format!("lock {}: {error}", self.state_dir.display()))
        };
        fs::create_dir_all(&self.state_dir).map_err(failed)?;
        let file = File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.state_dir.join("update.lock"))
            .map_err(failed)?;
        file.try_lock().map_err(|error| match error {
            fs::TryLockError::WouldBlock => Error::Install("another update is running".into()),
            fs::TryLockError::Error(error) => failed(error),
        })?;
        Ok(Lock { _file: file })
    }

    /// Start an update: forget the previous update's replaced binaries, so
    /// `--rollback` undoes this one only.
    pub fn begin(&self, _lock: &Lock) -> Result<(), Error> {
        let previous = self.previous_dir();
        match fs::remove_dir_all(&previous) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                return Err(Error::Install(format!(
                    "clear {}: {error}",
                    previous.display()
                )));
            }
            _ => {}
        }
        fs::create_dir_all(&previous)
            .map_err(|error| Error::Install(format!("create {}: {error}", previous.display())))
    }

    /// Put `contents` in place of `binary`, keeping the old file for
    /// [`Installer::rollback`].
    pub fn replace(&self, _lock: &Lock, binary: &str, contents: &[u8]) -> Result<(), Error> {
        let target = self.path(binary);
        if target.is_file() {
            let kept = self.previous_dir().join(binary);
            let staged = self.previous_dir().join(format!(".{binary}.keep"));
            fs::copy(&target, &staged)
                .and_then(|_| fs::rename(&staged, &kept))
                .map_err(|error| Error::Install(format!("keep the old {binary}: {error}")))?;
        }
        self.put(binary, |file| file.write_all(contents))
    }

    /// Write a new file beside `binary` and rename it over.
    fn put(
        &self,
        binary: &str,
        write: impl FnOnce(&mut File) -> std::io::Result<()>,
    ) -> Result<(), Error> {
        let target = self.path(binary);
        let staged = self.install_dir.join(format!(".{binary}.update"));
        let failed = |error: std::io::Error| {
            Error::Install(format!("install {}: {error}", target.display()))
        };
        let _ = fs::remove_file(&staged);
        let mut file = File::options()
            .write(true)
            .create_new(true)
            .open(&staged)
            .map_err(failed)?;
        let written = write(&mut file)
            .and_then(|()| file.sync_all())
            .and_then(|()| make_executable(&staged))
            .and_then(|()| fs::rename(&staged, &target));
        if let Err(error) = written {
            let _ = fs::remove_file(&staged);
            return Err(failed(error));
        }
        // The rename is durable once the directory is.
        if let Ok(dir) = File::open(&self.install_dir) {
            let _ = dir.sync_all();
        }
        Ok(())
    }

    /// The binaries the last update replaced, which [`Installer::rollback`]
    /// puts back.
    pub fn replaced(&self) -> Result<Vec<String>, Error> {
        let previous = self.previous_dir();
        let entries = match fs::read_dir(&previous) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(Error::Install(format!(
                    "read {}: {error}",
                    previous.display()
                )));
            }
        };
        let mut binaries: Vec<String> = entries
            .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
            .filter(|name| !name.starts_with('.'))
            .collect();
        binaries.sort();
        Ok(binaries)
    }

    /// Put back the binary the last update replaced. False when it replaced
    /// none under this name.
    pub fn rollback(&self, _lock: &Lock, binary: &str) -> Result<bool, Error> {
        let kept = self.previous_dir().join(binary);
        let mut old = match File::open(&kept) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(Error::Install(format!("read {}: {error}", kept.display()))),
        };
        self.put(binary, |file| std::io::copy(&mut old, file).map(drop))?;
        Ok(true)
    }

    /// A staging path for a download's check before it is installed: in the
    /// state directory, so nothing in the install directory sees it.
    pub fn staging(&self, binary: &str) -> PathBuf {
        self.state_dir.join("staging").join(binary)
    }
}

/// Mark a file executable by everyone, writable by its owner (`0755`).
pub fn make_executable(path: &Path) -> std::io::Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn installer() -> (tempfile::TempDir, Installer) {
        let dir = tempfile::tempdir().unwrap();
        let installer = Installer::new(dir.path().join("bin"), dir.path().join("state"));
        fs::create_dir_all(installer.install_dir()).unwrap();
        (dir, installer)
    }

    #[test]
    fn a_replaced_binary_is_kept_and_rolled_back() {
        let (_dir, installer) = installer();
        fs::write(installer.path("wuphf-server"), b"old").unwrap();
        let lock = installer.lock().unwrap();
        installer.begin(&lock).unwrap();
        installer.replace(&lock, "wuphf-server", b"new").unwrap();
        installer.replace(&lock, "wuphf-misc", b"first").unwrap();

        let path = installer.path("wuphf-server");
        assert_eq!(fs::read(&path).unwrap(), b"new");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o755
        );
        // A binary that was not there before has nothing to go back to.
        assert_eq!(installer.replaced().unwrap(), ["wuphf-server"]);
        assert!(installer.rollback(&lock, "wuphf-server").unwrap());
        assert!(!installer.rollback(&lock, "wuphf-misc").unwrap());
        assert_eq!(fs::read(&path).unwrap(), b"old");
        // Nothing staged is left behind.
        let left: Vec<_> = fs::read_dir(installer.install_dir())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(left.len(), 2, "{left:?}");
    }

    #[test]
    fn a_new_update_forgets_the_last_ones_replaced_binaries() {
        let (_dir, installer) = installer();
        fs::write(installer.path("wuphf-telegram"), b"old").unwrap();
        let lock = installer.lock().unwrap();
        installer.begin(&lock).unwrap();
        installer.replace(&lock, "wuphf-telegram", b"new").unwrap();
        installer.begin(&lock).unwrap();
        assert!(installer.replaced().unwrap().is_empty());
    }

    #[test]
    fn one_update_runs_at_a_time() {
        let (_dir, installer) = installer();
        let _lock = installer.lock().unwrap();
        let second = installer.lock().unwrap_err();
        assert!(
            second.to_string().contains("another update is running"),
            "{second}"
        );
    }
}
