//! Bearer-token handling.
//!
//! The API key is the single most sensitive value in the application. This
//! module confines every place it is exposed in plaintext:
//!
//! - It is stored in a [`SecretString`], which zeroizes on drop and is
//!   redacted from `Debug`.
//! - It is exposed only by [`ApiKey::authorization_header`] and
//!   [`ApiKey::expose_for_storage`]. Use `grep expose_` to audit every call
//!   site.
//! - The temporary `"Bearer …"` string is held in [`Zeroizing`], and the
//!   resulting header value is marked sensitive, so hyper and `tracing` do
//!   not render it.
//!
//! Limitation, best effort only: copies the HTTP stack makes while
//! serialising the request (hyper's write buffers, rustls records) are
//! outside our control and are not zeroized.

use std::fmt;

use reqwest::header::HeaderValue;
use secrecy::{ExposeSecret, SecretString};
use zeroize::Zeroizing;

use super::ApiError;

/// Shortest key accepted. Shorter values are almost certainly paste errors.
const MIN_KEY_LEN: usize = 8;
/// Longest key accepted. This bounds header size.
const MAX_KEY_LEN: usize = 512;

/// A validated Filester API key.
///
/// Validation allows only visible ASCII (`0x21..=0x7E`), which rules out
/// whitespace, control characters and CR/LF header injection before the
/// value gets near an HTTP request.
#[derive(Clone)]
pub struct ApiKey(SecretString);

impl ApiKey {
    /// Validate and wrap a key.
    ///
    /// Leading and trailing whitespace is trimmed, because keys are usually
    /// pasted from a browser.
    ///
    /// # Errors
    ///
    /// Returns [`ApiError::InvalidArgument`] if the trimmed key is shorter
    /// than 8 or longer than 512 characters, or contains anything other than
    /// visible ASCII. The error message never includes the key.
    pub fn new(raw: SecretString) -> Result<Self, ApiError> {
        let trimmed = raw.expose_secret().trim();
        if trimmed.len() < MIN_KEY_LEN || trimmed.len() > MAX_KEY_LEN {
            return Err(ApiError::InvalidArgument(format!(
                "API key must be between {MIN_KEY_LEN} and {MAX_KEY_LEN} characters"
            )));
        }
        if !trimmed.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(ApiError::InvalidArgument(
                "API key contains whitespace or non-printable characters".to_owned(),
            ));
        }
        if trimmed.len() == raw.expose_secret().len() {
            Ok(Self(raw))
        } else {
            Ok(Self(SecretString::from(trimmed)))
        }
    }

    /// Build the `Authorization: Bearer …` header value, marked sensitive.
    pub(crate) fn authorization_header(&self) -> Result<HeaderValue, ApiError> {
        let mut text = Zeroizing::new(String::with_capacity(7 + self.0.expose_secret().len()));
        text.push_str("Bearer ");
        text.push_str(self.0.expose_secret());
        let mut value = HeaderValue::from_str(&text).map_err(|_| {
            ApiError::InvalidArgument("API key is not a valid header value".to_owned())
        })?;
        value.set_sensitive(true);
        Ok(value)
    }

    /// Plaintext access for persisting the key in the Secret Service.
    ///
    /// The keyring is the only permitted consumer; see [`crate::secrets`].
    pub(crate) fn expose_for_storage(&self) -> &str {
        self.0.expose_secret()
    }
}

impl fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApiKey([REDACTED])")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(s: &str) -> Result<ApiKey, ApiError> {
        ApiKey::new(SecretString::from(s))
    }

    #[test]
    fn accepts_typical_key_and_trims_whitespace() {
        let k = key("  abcDEF123_-.~xyz \n").unwrap();
        assert_eq!(k.expose_for_storage(), "abcDEF123_-.~xyz");
    }

    #[test]
    fn rejects_short_long_and_non_printable() {
        assert!(key("short").is_err());
        assert!(key(&"a".repeat(MAX_KEY_LEN + 1)).is_err());
        assert!(key("abc def ghi").is_err());
        assert!(key("abcdefgh\r\nX-Injected: 1").is_err());
        assert!(key("abcdefgh\u{e9}").is_err());
    }

    #[test]
    fn error_messages_never_contain_the_key() {
        let secret = "abcdefgh ijklmnop";
        let err = key(secret).unwrap_err().to_string();
        assert!(!err.contains("abcdefgh"));
    }

    #[test]
    fn debug_is_redacted() {
        let k = key("supersecretvalue123").unwrap();
        let dbg = format!("{k:?}");
        assert!(!dbg.contains("supersecret"));
        assert!(dbg.contains("REDACTED"));
    }

    #[test]
    fn header_is_bearer_and_sensitive() {
        let k = key("tok_1234567890").unwrap();
        let h = k.authorization_header().unwrap();
        assert!(h.is_sensitive());
        assert_eq!(h.to_str().unwrap(), "Bearer tok_1234567890");
        assert!(!format!("{h:?}").contains("tok_"));
    }
}
