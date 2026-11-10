//! Streaming digest helpers.
//!
//! Every artifact the launcher downloads is verified by hash, so these helpers
//! must never load a whole file into memory (client jars and assets can be
//! hundreds of megabytes).

use std::path::Path;

use md5::Md5;
use serde::{Deserialize, Serialize};
use sha1::Sha1;
use sha2::{Digest, Sha256, Sha512};

use crate::error::Result;

/// Hash algorithm used to verify a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HashAlgorithm {
    Md5,
    Sha1,
    Sha256,
    Sha512,
}

impl HashAlgorithm {
    /// Parse a digest length in hex characters into an algorithm.
    pub fn from_hex_len(len: usize) -> Option<Self> {
        match len {
            32 => Some(Self::Md5),
            40 => Some(Self::Sha1),
            64 => Some(Self::Sha256),
            128 => Some(Self::Sha512),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Md5 => "md5",
            Self::Sha1 => "sha1",
            Self::Sha256 => "sha256",
            Self::Sha512 => "sha512",
        }
    }
}

fn digest_of(algorithm: HashAlgorithm, bytes: &[u8]) -> String {
    match algorithm {
        HashAlgorithm::Md5 => hex::encode(Md5::digest(bytes)),
        HashAlgorithm::Sha1 => hex::encode(Sha1::digest(bytes)),
        HashAlgorithm::Sha256 => hex::encode(Sha256::digest(bytes)),
        HashAlgorithm::Sha512 => hex::encode(Sha512::digest(bytes)),
    }
}

/// Hash an in-memory buffer, lowered for comparison against API digests.
pub fn hash_bytes(algorithm: HashAlgorithm, bytes: &[u8]) -> String {
    digest_of(algorithm, bytes)
}

/// Hash a file in fixed-size chunks.
pub async fn hash_file(algorithm: HashAlgorithm, path: &Path) -> Result<String> {
    use tokio::io::AsyncReadExt;

    let mut file = tokio::fs::File::open(path).await?;
    let mut buffer = vec![0u8; 64 * 1024];

    macro_rules! read_into {
        ($hasher:expr) => {{
            let mut hasher = $hasher;
            loop {
                let read = file.read(&mut buffer).await?;
                if read == 0 {
                    break;
                }
                hasher.update(&buffer[..read]);
            }
            hex::encode(hasher.finalize())
        }};
    }

    Ok(match algorithm {
        HashAlgorithm::Md5 => read_into!(Md5::new()),
        HashAlgorithm::Sha1 => read_into!(Sha1::new()),
        HashAlgorithm::Sha256 => read_into!(Sha256::new()),
        HashAlgorithm::Sha512 => read_into!(Sha512::new()),
    })
}

/// Hash an async byte stream, returning the digest and the byte count.
pub async fn hash_stream<S, E>(algorithm: HashAlgorithm, mut stream: S) -> Result<(String, u64)>
where
    S: futures_util::Stream<Item = std::result::Result<bytes::Bytes, E>> + Unpin,
    E: std::fmt::Display,
{
    use futures_util::StreamExt;

    let mut count = 0u64;

    macro_rules! consume {
        ($hasher:expr) => {{
            let mut hasher = $hasher;
            while let Some(chunk) = stream.next().await {
                let chunk =
                    chunk.map_err(|err| crate::error::MangoError::Other(err.to_string()))?;
                hasher.update(&chunk);
                count += chunk.len() as u64;
            }
            (hex::encode(hasher.finalize()), count)
        }};
    }

    let result = match algorithm {
        HashAlgorithm::Md5 => consume!(Md5::new()),
        HashAlgorithm::Sha1 => consume!(Sha1::new()),
        HashAlgorithm::Sha256 => consume!(Sha256::new()),
        HashAlgorithm::Sha512 => consume!(Sha512::new()),
    };
    Ok(result)
}

/// `true` when `path` exists and matches `expected` (case-insensitive hex).
pub async fn verify_file(algorithm: HashAlgorithm, path: &Path, expected: &str) -> Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    let actual = hash_file(algorithm, path).await?;
    Ok(actual.eq_ignore_ascii_case(expected))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_match_known_vectors() {
        // Vectors from the respective RFCs.
        assert_eq!(hash_bytes(HashAlgorithm::Md5, b"abc"), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(
            hash_bytes(HashAlgorithm::Sha1, b"abc"),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
        assert_eq!(
            hash_bytes(HashAlgorithm::Sha256, b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn algorithm_is_inferred_from_digest_length() {
        assert_eq!(HashAlgorithm::from_hex_len(32), Some(HashAlgorithm::Md5));
        assert_eq!(HashAlgorithm::from_hex_len(40), Some(HashAlgorithm::Sha1));
        assert_eq!(HashAlgorithm::from_hex_len(64), Some(HashAlgorithm::Sha256));
        assert_eq!(HashAlgorithm::from_hex_len(128), Some(HashAlgorithm::Sha512));
        assert_eq!(HashAlgorithm::from_hex_len(41), None);
    }

    #[tokio::test]
    async fn file_hashing_matches_memory_hashing() {
        let path = std::env::temp_dir().join(format!("mango-hash-{}.bin", std::process::id()));
        let payload = vec![0x5au8; 200_000];
        tokio::fs::write(&path, &payload).await.unwrap();

        let from_file = hash_file(HashAlgorithm::Sha256, &path).await.unwrap();
        assert_eq!(from_file, hash_bytes(HashAlgorithm::Sha256, &payload));

        assert!(
            verify_file(HashAlgorithm::Sha256, &path, &from_file.to_uppercase()).await.unwrap()
        );
        assert!(!verify_file(HashAlgorithm::Sha256, &path, "deadbeef").await.unwrap());

        tokio::fs::remove_file(&path).await.ok();
    }
}
