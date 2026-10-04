//! Photo identities (CG-15). PII: a URI and photo bytes never reach `Debug`,
//! `Display`, logs or errors.

use std::{fmt, sync::Arc};

use sha2::{Digest, Sha256};

/// sha256 of a photo's decoded bytes.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct PhotoHash([u8; 32]);

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("invalid photo hash")]
pub struct PhotoHashError;

impl PhotoHash {
    #[must_use]
    pub fn of(bytes: &[u8]) -> Self {
        Self(Sha256::digest(bytes).into())
    }

    #[must_use]
    pub fn as_hex(&self) -> String {
        use fmt::Write as _;
        self.0.iter().fold(String::with_capacity(64), |mut out, b| {
            let _ = write!(out, "{b:02x}");
            out
        })
    }

    pub fn from_hex(hex: &str) -> Result<Self, PhotoHashError> {
        if hex.len() != 64 {
            return Err(PhotoHashError);
        }
        let mut out = [0u8; 32];
        for (i, byte) in out.iter_mut().enumerate() {
            *byte = u8::from_str_radix(hex.get(2 * i..2 * i + 2).ok_or(PhotoHashError)?, 16).map_err(|_| PhotoHashError)?;
        }
        Ok(Self(out))
    }
}

impl fmt::Debug for PhotoHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PhotoHash({}…)", &self.as_hex()[..12])
    }
}

/// A `PHOTO;VALUE=uri:` value (an iCloud gateway URL). PII.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct PhotoUri(String);

impl PhotoUri {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for PhotoUri {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl fmt::Debug for PhotoUri {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PhotoUri(<redacted>)")
    }
}

/// Decoded photo bytes and their hash. PII: `Debug` prints the hash only.
#[derive(Clone, PartialEq, Eq)]
pub struct PhotoData {
    pub hash: PhotoHash,
    pub bytes: Arc<[u8]>,
}

impl PhotoData {
    #[must_use]
    pub fn new(bytes: Vec<u8>) -> Self {
        Self {
            hash: PhotoHash::of(&bytes),
            bytes: bytes.into(),
        }
    }
}

impl fmt::Debug for PhotoData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PhotoData").field("hash", &self.hash).finish_non_exhaustive()
    }
}

/// A card's first `PHOTO` property, as written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CardPhoto {
    /// `ENCODING=b` (or `BASE64`) data, decoded.
    Inline(PhotoData),
    /// `VALUE=uri`, or a value that is an http(s) URL.
    Uri(PhotoUri),
    /// Neither: base64 that does not decode, or an unknown form.
    Unreadable,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_round_trips_through_hex_and_debug_hides_the_uri() {
        let hash = PhotoHash::of(b"abc");
        assert_eq!(PhotoHash::from_hex(&hash.as_hex()).unwrap(), hash);
        assert_eq!(PhotoHash::from_hex("zz"), Err(PhotoHashError));
        let uri = PhotoUri::from("https://gateway.icloud.com/x/y".to_owned());
        assert_eq!(format!("{uri:?}"), "PhotoUri(<redacted>)");
        let data = PhotoData::new(b"abc".to_vec());
        assert!(!format!("{data:?}").contains("abc\""), "bytes never in Debug");
        assert_eq!(data.hash, hash);
    }
}
