//! Typed errors for the API engine.
//!
//! Server-supplied text (error messages, codes) is untrusted. Before it is
//! stored in an error it goes through [`sanitize_server_text`], which strips
//! control characters and bounds the length, so it is safe to log and to show
//! in a label.

use std::time::Duration;

use thiserror::Error;

/// Maximum number of characters of server-supplied text kept in an error.
const MAX_SERVER_TEXT_CHARS: usize = 300;

/// Every way an API operation can fail.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ApiError {
    /// The client was constructed with an unusable configuration.
    #[error("invalid client configuration: {0}")]
    Config(String),

    /// A caller-supplied argument failed validation before any request was sent.
    #[error("invalid argument: {0}")]
    InvalidArgument(String),

    /// The operation needs an API key, but the client was built without one.
    #[error("not signed in: this operation requires an API key")]
    NotSignedIn,

    /// HTTP 401: the API key is missing, invalid or was regenerated.
    #[error("the API key was rejected")]
    Unauthorized,

    /// HTTP 403.
    #[error("permission denied: {message}")]
    Forbidden {
        /// Sanitised server message.
        message: String,
    },

    /// HTTP 404.
    #[error("not found: {message}")]
    NotFound {
        /// Sanitised server message.
        message: String,
    },

    /// HTTP 400 or 422.
    #[error("request rejected: {message}")]
    BadRequest {
        /// Sanitised server message.
        message: String,
    },

    /// HTTP 429 after retries were exhausted, or a wait longer than
    /// [`RetryPolicy::max_wait`](super::RetryPolicy::max_wait) was required.
    #[error("rate limit exceeded")]
    RateLimited {
        /// How long the server asked us to wait, if it said.
        retry_after: Option<Duration>,
    },

    /// HTTP 5xx.
    #[error("server error (HTTP {status}): {message}")]
    Server {
        /// HTTP status code.
        status: u16,
        /// Sanitised server message.
        message: String,
    },

    /// Any other non-success HTTP status.
    #[error("unexpected HTTP status {status}: {message}")]
    UnexpectedStatus {
        /// HTTP status code.
        status: u16,
        /// Sanitised server message.
        message: String,
    },

    /// HTTP 200 with `"success": false` in the body.
    #[error("the server reported a failure: {message}")]
    Rejected {
        /// Sanitised server message.
        message: String,
        /// Machine-readable error code, for example `FOLDER_NOT_FOUND`.
        code: Option<String>,
    },

    /// The server redirected to a different origin. The redirect is refused
    /// so the bearer token can never be forwarded to another host.
    #[error("refused to follow a redirect away from the API origin")]
    CrossOriginRedirect,

    /// A connect, read or response timeout elapsed.
    #[error("request timed out")]
    Timeout,

    /// An upload made no progress for the configured idle window.
    #[error("upload stalled: no data was accepted for {} s", .0.as_secs())]
    Stalled(Duration),

    /// Transport-level failure (DNS, TLS, connection reset, …). The request
    /// URL is stripped from the inner error.
    #[error("network error: {0}")]
    Network(#[source] reqwest::Error),

    /// The response body exceeded the configured cap.
    #[error("response exceeded {limit} bytes")]
    ResponseTooLarge {
        /// The cap that was exceeded, in bytes.
        limit: usize,
    },

    /// The response was not the JSON shape we expected.
    #[error("malformed response: {0}")]
    Decode(String),

    /// The local file exceeds the server's upload limit.
    #[error("file is {size} bytes; the limit is {limit} bytes")]
    FileTooLarge {
        /// Local file size in bytes.
        size: u64,
        /// Server limit in bytes.
        limit: u64,
    },

    /// Reading a local file failed, or it changed while being uploaded.
    #[error("local file error: {0}")]
    Io(#[from] std::io::Error),
}

impl ApiError {
    /// Classify a transport error.
    ///
    /// The URL is stripped (`without_url`) so search terms and identifiers in
    /// query strings never reach logs or the UI.
    pub(crate) fn from_reqwest(err: reqwest::Error) -> Self {
        if err.is_timeout() {
            return Self::Timeout;
        }
        if err.is_redirect() && has_source::<CrossOriginRedirect>(&err) {
            return Self::CrossOriginRedirect;
        }
        Self::Network(err.without_url())
    }
}

/// Marker error raised from the redirect policy and recognised by
/// [`ApiError::from_reqwest`].
#[derive(Debug, Error)]
#[error("cross-origin redirect refused")]
pub(crate) struct CrossOriginRedirect;

fn has_source<E: std::error::Error + 'static>(err: &(dyn std::error::Error + 'static)) -> bool {
    let mut current = err.source();
    while let Some(source) = current {
        if source.is::<E>() {
            return true;
        }
        current = source.source();
    }
    false
}

/// Make untrusted server text safe to log and display.
///
/// Control characters (including newlines, which could forge log lines) are
/// replaced with spaces, runs of whitespace are collapsed, and the result is
/// truncated to [`MAX_SERVER_TEXT_CHARS`] characters.
pub(crate) fn sanitize_server_text(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len().min(MAX_SERVER_TEXT_CHARS * 4));
    let mut count = 0usize;
    let mut last_space = true;
    for ch in raw.chars() {
        let ch = if ch.is_control() || ch.is_whitespace() {
            ' '
        } else {
            ch
        };
        if ch == ' ' {
            if last_space {
                continue;
            }
            last_space = true;
        } else {
            last_space = false;
        }
        if count == MAX_SERVER_TEXT_CHARS {
            out.push('…');
            break;
        }
        out.push(ch);
        count += 1;
    }
    let trimmed_len = out.trim_end().len();
    out.truncate(trimmed_len);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_strips_controls_and_collapses_whitespace() {
        assert_eq!(sanitize_server_text("  bad\r\n\tthing\u{7}  "), "bad thing");
    }

    #[test]
    fn sanitize_truncates_on_char_boundary() {
        let long = "é".repeat(MAX_SERVER_TEXT_CHARS + 50);
        let out = sanitize_server_text(&long);
        assert_eq!(out.chars().count(), MAX_SERVER_TEXT_CHARS + 1);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn sanitize_empty_is_empty() {
        assert_eq!(sanitize_server_text(" \n "), "");
    }

    #[test]
    fn stalled_display_uses_seconds() {
        assert_eq!(
            ApiError::Stalled(Duration::from_secs(120)).to_string(),
            "upload stalled: no data was accepted for 120 s"
        );
    }
}
