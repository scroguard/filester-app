//! User-facing text: error descriptions, sizes, and safe use of
//! markup-interpreting widgets.

use std::time::Duration;

use filester_app::api::ApiError;
use gtk::glib;
use url::Url;

/// Plain-language description of an API error, suitable for a toast or label.
pub fn describe_error(err: &ApiError) -> String {
    match err {
        ApiError::Unauthorized => {
            "Your API key was rejected. Sign in again with a current key.".to_owned()
        }
        ApiError::NotSignedIn => "You are not signed in.".to_owned(),
        ApiError::RateLimited {
            retry_after: Some(wait),
        } => format!(
            "Filester's rate limit was reached. Try again in {}.",
            human_duration(*wait)
        ),
        ApiError::RateLimited { retry_after: None } => {
            "Filester's rate limit was reached. Try again in a few minutes.".to_owned()
        }
        ApiError::Network(_) => {
            "Couldn't reach Filester. Check your internet connection.".to_owned()
        }
        ApiError::Timeout => "Filester took too long to respond.".to_owned(),
        ApiError::Stalled(_) => "The upload stopped making progress and was cancelled.".to_owned(),
        ApiError::FileTooLarge { limit, .. } => {
            format!(
                "This file is larger than the {} upload limit.",
                glib::format_size(*limit)
            )
        }
        ApiError::CrossOriginRedirect => {
            "Filester redirected to an unexpected site, so the request was blocked.".to_owned()
        }
        ApiError::Io(e) => format!("Couldn't read the file: {e}"),
        other => {
            let text = other.to_string();
            let mut chars = text.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_uppercase().chain(chars).collect()
            })
        }
    }
}

/// "45 seconds", "3 minutes", "2 hours", rounded up.
pub fn human_duration(d: Duration) -> String {
    let secs = d.as_secs() + u64::from(d.subsec_nanos() > 0);
    let (n, unit) = if secs < 60 {
        (secs.max(1), "second")
    } else if secs < 3600 {
        (secs.div_ceil(60), "minute")
    } else {
        (secs.div_ceil(3600), "hour")
    };
    format!("{n} {unit}{}", if n == 1 { "" } else { "s" })
}

/// Whether a server-supplied share URL may be opened or copied: `https`, a
/// `filester.me` host or subdomain, and no embedded credentials.
pub fn is_safe_share_url(raw: &str) -> bool {
    let Ok(url) = Url::parse(raw) else {
        return false;
    };
    let host_ok = url
        .host_str()
        .is_some_and(|h| h == "filester.me" || h.ends_with(".filester.me"));
    url.scheme() == "https" && host_ok && url.username().is_empty() && url.password().is_none()
}

/// A toast that renders its text literally. `AdwToast:title` is Pango markup
/// by default, so a file named `<b>x` would otherwise be interpreted.
pub fn plain_toast(text: &str) -> adw::Toast {
    let toast = adw::Toast::new(text);
    toast.set_use_markup(false);
    toast
}

/// Set an `AdwStatusPage` description, which is always Pango markup, from
/// untrusted text.
pub fn set_status_description(page: &adw::StatusPage, text: &str) {
    page.set_description(Some(&glib::markup_escape_text(text)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_round_up_and_pluralise() {
        assert_eq!(human_duration(Duration::from_millis(200)), "1 second");
        assert_eq!(human_duration(Duration::from_secs(45)), "45 seconds");
        assert_eq!(human_duration(Duration::from_secs(61)), "2 minutes");
        assert_eq!(human_duration(Duration::from_secs(3600)), "1 hour");
        assert_eq!(human_duration(Duration::from_secs(3601)), "2 hours");
    }

    #[test]
    fn share_url_validation() {
        assert!(is_safe_share_url("https://filester.me/d/aBcDeFg"));
        assert!(is_safe_share_url("https://u1.filester.me/d/x"));
        assert!(!is_safe_share_url("http://filester.me/d/x"));
        assert!(!is_safe_share_url("https://filester.me.evil.example/d/x"));
        assert!(!is_safe_share_url("https://evilfilester.me/d/x"));
        assert!(!is_safe_share_url("https://user:pw@filester.me/d/x"));
        assert!(!is_safe_share_url("javascript:alert(1)"));
        assert!(!is_safe_share_url("/d/relative"));
    }

    #[test]
    fn errors_are_readable() {
        assert!(describe_error(&ApiError::Unauthorized).contains("rejected"));
        assert_eq!(
            describe_error(&ApiError::RateLimited {
                retry_after: Some(Duration::from_secs(90))
            }),
            "Filester's rate limit was reached. Try again in 2 minutes."
        );
        assert_eq!(
            describe_error(&ApiError::NotFound {
                message: "gone".into()
            }),
            "Not found: gone"
        );
    }
}
