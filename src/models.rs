//! Local model files: downloaded at runtime into `%LOCALAPPDATA%\CluelyRS\models`, never
//! bundled with the app or committed. Downloads are pinned to an exact revision and verified
//! by size and SHA-256 before they're used.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, bail};
use sha2::{Digest, Sha256};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModelFile {
    /// Subdirectory under the models directory.
    pub family: &'static str,
    pub name: &'static str,
    /// Download URL pinned to a specific revision.
    pub url: &'static str,
    pub bytes: u64,
    /// Lowercase hex SHA-256 of the file.
    pub sha256: &'static str,
    /// Where the license terms are, shown before downloading.
    pub license_url: &'static str,
}

pub fn models_dir() -> Option<PathBuf> {
    dirs::data_local_dir().map(|dir| dir.join("CluelyRS").join("models"))
}

impl ModelFile {
    pub fn path(&self) -> Option<PathBuf> { models_dir().map(|dir| dir.join(self.family).join(self.name)) }

    /// Present with the expected size. (The hash is checked once, when downloading.)
    pub fn is_installed_at(&self, path: &Path) -> bool {
        fs::metadata(path).is_ok_and(|meta| meta.is_file() && meta.len() == self.bytes)
    }

    /// Download into `dest`, reporting `(received, total)` bytes. Writes to a `.part` file and
    /// renames it only after the size and hash match; returns early (with an error) if `cancel`
    /// is set. Blocking: call from a background thread.
    pub fn download_to(&self, dest: &Path, cancel: &AtomicBool, mut progress: impl FnMut(u64, u64)) -> anyhow::Result<()> {
        let dir = dest.parent().context("model path has no parent directory")?;
        fs::create_dir_all(dir).with_context(|| format!("couldn't create {}", dir.display()))?;
        let part = dest.with_extension("part");
        let result = self.fetch(&part, cancel, &mut progress);
        if result.is_err() { let _ = fs::remove_file(&part); }
        result?;
        fs::rename(&part, dest).with_context(|| format!("couldn't move the model into {}", dest.display()))
    }

    fn fetch(&self, part: &Path, cancel: &AtomicBool, progress: &mut impl FnMut(u64, u64)) -> anyhow::Result<()> {
        let response = ureq::get(self.url).call().with_context(|| format!("couldn't download {}", self.name))?;
        let mut body = response.into_body().into_with_config().limit(self.bytes + 1).reader();
        let mut file = File::create(part).with_context(|| format!("couldn't create {}", part.display()))?;
        let mut hasher = Sha256::new();
        let mut buffer = vec![0u8; 1 << 16];
        let mut received = 0u64;
        loop {
            if cancel.load(Ordering::Relaxed) { bail!("download cancelled"); }
            let n = body.read(&mut buffer).context("download interrupted")?;
            if n == 0 { break; }
            received += n as u64;
            if received > self.bytes { bail!("{} is larger than expected", self.name); }
            hasher.update(&buffer[..n]);
            file.write_all(&buffer[..n]).context("couldn't write the model file")?;
            progress(received, self.bytes);
        }
        file.sync_all().context("couldn't write the model file")?;
        verify(self, received, &format!("{:x}", hasher.finalize()))
    }
}

fn verify(file: &ModelFile, received: u64, sha256: &str) -> anyhow::Result<()> {
    if received != file.bytes { bail!("{} is incomplete ({received} of {} bytes)", file.name, file.bytes); }
    if !sha256.eq_ignore_ascii_case(file.sha256) { bail!("{} failed its integrity check", file.name); }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: ModelFile = ModelFile { family: "test", name: "m.gguf", url: "https://example.invalid/m.gguf", bytes: 3,
        sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad", license_url: "" };

    #[test]
    fn verification_needs_the_exact_size_and_hash() {
        let abc = format!("{:x}", Sha256::digest(b"abc"));
        assert!(verify(&FILE, 3, &abc).is_ok());
        assert!(verify(&FILE, 2, &abc).is_err());
        assert!(verify(&FILE, 3, &format!("{:x}", Sha256::digest(b"abd"))).is_err());
    }

    #[test]
    fn installed_means_present_with_the_expected_size() {
        let dir = std::env::temp_dir().join(format!("cluelyrs-models-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("m.gguf");
        assert!(!FILE.is_installed_at(&path));
        fs::write(&path, b"ab").unwrap();
        assert!(!FILE.is_installed_at(&path));
        fs::write(&path, b"abc").unwrap();
        assert!(FILE.is_installed_at(&path));
        fs::remove_dir_all(&dir).unwrap();
    }
}
