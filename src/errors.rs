//! Turning yt-dlp's output into a sentence a person can act on.

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct Failure {
    /// Plain-language summary for the page.
    pub message: String,
    /// The raw error line(s), behind "Details".
    pub detail: String,
    /// "cookies" when uploading a cookies.txt would likely fix it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<&'static str>,
}

impl Failure {
    pub fn new(message: impl Into<String>, detail: impl Into<String>) -> Self {
        Self { message: message.into(), detail: detail.into(), hint: None }
    }
    pub fn internal(detail: impl Into<String>) -> Self {
        Self::new("Something went wrong on the server.", detail)
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.detail.is_empty() {
            write!(f, "{}", self.message)
        } else {
            write!(f, "{} ({})", self.message, self.detail)
        }
    }
}

const SIGN_IN: &[&str] = &["sign in to confirm", "confirm your age", "age-restricted", "login required", "log in", "you need to log in", "cookies", "private account"];

const RULES: &[(&[&str], &str)] = &[
    (&["private video", "video unavailable", "has been removed", "no longer available", "does not exist", "http error 404", "account has been terminated"],
        "This video is private or was removed."),
    (&["unsupported url"], "That site isn't supported."),
    (&["http error 429", "too many requests", "rate-limit", "rate limit"],
        "The site is rate-limiting the server. Try again in a few minutes."),
    (&["max-filesize", "larger than max-filesize"], "The file is bigger than the server's size limit."),
    (&["requested format is not available", "requested format not available"],
        "That format isn't available any more. Fetch the options again."),
    (&["no video formats found", "no video could be found", "no media found", "there's no video"],
        "There's no video at that link."),
    (&["not a valid url", "is not a valid url"], "That doesn't look like a link."),
    (&["timed out", "connection reset", "unable to download webpage", "name or service not known", "network is unreachable"],
        "Couldn't reach the site. Try again."),
    (&["drm"], "This video is DRM-protected and can't be downloaded."),
    (&["live event will begin", "premieres in", "is not currently live"], "This stream hasn't started yet."),
];

/// Picks the most relevant line (the last ERROR, else the last line) and matches it.
pub fn explain<'a>(lines: impl IntoIterator<Item = &'a str>, user: &str) -> Failure {
    let lines: Vec<&str> = lines.into_iter().map(str::trim).filter(|l| !l.is_empty()).collect();
    let detail = lines
        .iter()
        .rev()
        .find(|l| l.starts_with("ERROR"))
        .or_else(|| lines.iter().rev().find(|l| l.contains("max-filesize")))
        .or(lines.last())
        .map(|l| l.trim_start_matches("ERROR:").trim().to_string())
        .unwrap_or_default();
    let lower = detail.to_lowercase();
    if SIGN_IN.iter().any(|n| lower.contains(n)) && !lower.contains("private video") {
        let message = if crate::config::user_cookies(user).exists() {
            "This needs a signed-in account, and your cookies didn't work. They may have expired: export and upload them again in Settings."
        } else {
            "This needs a signed-in account. Upload your own cookies.txt in Settings to use yours."
        };
        return Failure { hint: Some("cookies"), ..Failure::new(message, detail) };
    }
    let message = RULES
        .iter()
        .find(|(needles, _)| needles.iter().any(|n| lower.contains(n)))
        .map(|(_, m)| *m)
        .unwrap_or("The download failed.");
    Failure::new(message, detail)
}

#[cfg(test)]
mod tests {
    use super::explain;

    #[test]
    fn sign_in_points_at_cookies() {
        let f = explain(["[youtube] x: Downloading webpage", "ERROR: [youtube] x: Sign in to confirm you're not a bot. Use --cookies-from-browser or --cookies for the authentication."], "nobody-here");
        assert_eq!(f.hint, Some("cookies"));
        assert!(f.message.contains("Upload your own cookies.txt"));
    }

    #[test]
    fn private_video_is_not_a_cookie_problem() {
        let f = explain(["ERROR: [youtube] x: Private video. Sign in if you've been granted access to this video"], "nobody-here");
        assert_eq!(f.hint, None);
        assert_eq!(f.message, "This video is private or was removed.");
    }

    #[test]
    fn rate_limit() {
        let f = explain(["ERROR: unable to download video data: HTTP Error 429: Too Many Requests"], "nobody-here");
        assert!(f.message.contains("rate-limiting"));
    }
}
