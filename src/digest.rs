use crate::error::{Result, WaxError};
use sha2::Digest;
use std::io::Read;
use std::path::Path;
use tracing::{debug, warn};

pub fn sha256_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

pub fn sha256_digest_hex(data: impl AsRef<[u8]>) -> String {
    let out = sha2::Sha256::digest(data);
    sha256_hex(&out)
}

/// Verify a file against an expected SHA256 hex digest.
///
/// Homebrew uses `"no_check"` to skip verification; wax logs a warning when that happens.
pub fn verify_sha256_file(path: &Path, expected_sha256: &str) -> Result<()> {
    if expected_sha256 == "no_check" {
        warn!("Skipping checksum verification (no_check) for {:?}", path);
        eprintln!(
            "warning: skipping checksum verification (no_check) for {}",
            path.display()
        );
        return Ok(());
    }

    debug!("Verifying checksum for {:?}", path);

    let mut file = std::fs::File::open(path)?;
    let mut hasher = sha2::Sha256::new();
    let mut buffer = [0u8; 8192];

    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }

    let hash = sha256_hex(&hasher.finalize());

    if hash != expected_sha256 {
        return Err(WaxError::ChecksumMismatch {
            expected: expected_sha256.to_string(),
            actual: hash,
        });
    }

    debug!("Checksum verified: {}", hash);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_verify_sha256_file_success() {
        let mut file = NamedTempFile::new().unwrap();
        let data = b"hello world";
        file.write_all(data).unwrap();

        let hash = sha256_digest_hex(data);
        assert!(verify_sha256_file(file.path(), &hash).is_ok());
    }

    #[test]
    fn test_verify_sha256_file_mismatch() {
        let mut file = NamedTempFile::new().unwrap();
        let data = b"hello world";
        file.write_all(data).unwrap();

        let expected_hash = "wronghash";
        let err = verify_sha256_file(file.path(), expected_hash).unwrap_err();

        match err {
            WaxError::ChecksumMismatch { expected, actual } => {
                assert_eq!(expected, "wronghash");
                assert_eq!(actual, sha256_digest_hex(data));
            }
            _ => panic!("Expected ChecksumMismatch error"),
        }
    }

    #[test]
    fn test_verify_sha256_file_no_check() {
        let path = Path::new("does_not_exist.txt");
        assert!(verify_sha256_file(path, "no_check").is_ok());
    }

    #[test]
    fn test_verify_sha256_file_not_found() {
        let path = Path::new("does_not_exist.txt");
        let err = verify_sha256_file(path, "somehash").unwrap_err();

        match err {
            WaxError::IoError(_) => {}
            _ => panic!("Expected IoError"),
        }
    }
}
