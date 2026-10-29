use std::fs::File;
use std::io::Read;
use std::path::Path;

use md5::Md5;
use sha1::Sha1;
use sha2::{Digest, Sha256, Sha512};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HashAlgo {
    Md5,
    Sha1,
    Sha256,
    Sha512,
}

/// Split a manifest hash spec into (algorithm, hex_digest).
///
/// Scoop manifests express hashes either as bare hex (algorithm inferred
/// from length: 32=md5, 40=sha1, 64=sha256, 128=sha512) or as
/// `"algo:hexdigest"` (e.g. `"sha512:abcd..."`). Rake previously ALWAYS
/// hashed downloads with SHA256 regardless of what the manifest asked
/// for, which made every package pinned to sha512 (or md5/sha1) fail
/// verification 100% of the time — this was the root cause of
/// "hash mismatch" errors during install/update.
pub fn parse_hash_spec(spec: &str) -> (HashAlgo, &str) {
    if let Some((prefix, rest)) = spec.split_once(':') {
        let algo = match prefix.to_ascii_lowercase().as_str() {
            "md5" => HashAlgo::Md5,
            "sha1" => HashAlgo::Sha1,
            "sha256" => HashAlgo::Sha256,
            "sha512" => HashAlgo::Sha512,
            _ => HashAlgo::Sha256,
        };
        (algo, rest)
    } else {
        let algo = match spec.len() {
            32 => HashAlgo::Md5,
            40 => HashAlgo::Sha1,
            128 => HashAlgo::Sha512,
            _ => HashAlgo::Sha256,
        };
        (algo, spec)
    }
}

fn hash_reader<D: Digest, R: Read>(reader: &mut R) -> std::io::Result<String> {
    let mut hasher = D::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Compute the cryptographic digest of a file using the specified algorithm.
pub fn compute_file_hash(path: &Path, algo: HashAlgo) -> std::io::Result<String> {
    let mut file = File::open(path)?;
    match algo {
        HashAlgo::Md5 => hash_reader::<Md5, _>(&mut file),
        HashAlgo::Sha1 => hash_reader::<Sha1, _>(&mut file),
        HashAlgo::Sha256 => hash_reader::<Sha256, _>(&mut file),
        HashAlgo::Sha512 => hash_reader::<Sha512, _>(&mut file),
    }
}

/// Verify that a file's hash matches the expected manifest hash spec.
///
/// Supports md5 / sha1 / sha256 / sha512, with or without an `"algo:"`
/// prefix (see [`parse_hash_spec`]). Returns `Ok(())` on match, or an
/// error on mismatch / I/O failure. Hash comparison is case-insensitive.
pub fn verify_file_hash(path: &Path, expected: &str) -> std::io::Result<()> {
    let (algo, expected_hex) = parse_hash_spec(expected);
    let actual = compute_file_hash(path, algo)?;

    if actual.eq_ignore_ascii_case(expected_hex) {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "hash mismatch: expected {expected}, got {actual}"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infers_sha256_from_bare_64_char_hex() {
        let spec = "a".repeat(64);
        assert_eq!(parse_hash_spec(&spec).0, HashAlgo::Sha256);
    }

    #[test]
    fn infers_sha512_from_bare_128_char_hex() {
        let spec = "a".repeat(128);
        assert_eq!(parse_hash_spec(&spec).0, HashAlgo::Sha512);
    }

    #[test]
    fn infers_md5_from_bare_32_char_hex() {
        let spec = "a".repeat(32);
        assert_eq!(parse_hash_spec(&spec).0, HashAlgo::Md5);
    }

    #[test]
    fn infers_sha1_from_bare_40_char_hex() {
        let spec = "a".repeat(40);
        assert_eq!(parse_hash_spec(&spec).0, HashAlgo::Sha1);
    }

    #[test]
    fn respects_explicit_sha512_prefix() {
        let spec = format!("sha512:{}", "a".repeat(128));
        let (algo, hex) = parse_hash_spec(&spec);
        assert_eq!(algo, HashAlgo::Sha512);
        assert_eq!(hex, "a".repeat(128));
    }

    #[test]
    fn verifies_known_sha256_vector() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.bin");
        std::fs::write(&path, b"hello world").unwrap();
        let expected = "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9";
        verify_file_hash(&path, expected).unwrap();
    }

    #[test]
    fn verifies_known_sha512_prefixed_vector() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.bin");
        std::fs::write(&path, b"hello world").unwrap();
        let expected = "sha512:309ecc489c12d6eb4cc40f50c902f2b4d0ed77ee511a7c7a9bcd3ca86d4cd86f989dd35bc5ff499670da34255b45b0cfd830e81f605dcf7dc5542e93ae9cd76f";
        verify_file_hash(&path, expected).unwrap();
    }

    #[test]
    fn rejects_wrong_hash() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.bin");
        std::fs::write(&path, b"hello world").unwrap();
        let wrong = "0".repeat(64);
        assert!(verify_file_hash(&path, &wrong).is_err());
    }
}
