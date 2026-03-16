//! # URL Credential Stripping
//!
//! Prevents accidental credential leakage by stripping userinfo (username:password)
//! from URLs before they are logged, stored, or displayed.
//!
//! Handles standard URI schemes: `http://`, `https://`, `mongodb://`, `postgresql://`,
//! `redis://`, `amqp://`, and any other `scheme://user:pass@host` format.
//!
//! Inspired by openclaw's `url-userinfo.ts`.

/// Strip userinfo (username/password) from a URL.
///
/// Transforms `scheme://user:pass@host/path` → `scheme://host/path`.
/// If the URL has no userinfo, returns the input unchanged.
///
/// # Examples
///
/// ```rust
/// use clawdesk_security::url_sanitize::strip_url_userinfo;
///
/// assert_eq!(
///     strip_url_userinfo("mongodb://admin:s3cret@db.example.com:27017/mydb"),
///     "mongodb://db.example.com:27017/mydb"
/// );
///
/// assert_eq!(
///     strip_url_userinfo("https://example.com/path"),
///     "https://example.com/path"
/// );
/// ```
pub fn strip_url_userinfo(url: &str) -> String {
    // Find the `://` separator.
    let Some(scheme_end) = url.find("://") else {
        return url.to_string();
    };

    let after_scheme = scheme_end + 3; // skip `://`
    let rest = &url[after_scheme..];

    // Find the first `/`, `?`, or `#` that ends the authority section.
    let authority_end = rest
        .find(|c: char| c == '/' || c == '?' || c == '#')
        .unwrap_or(rest.len());

    let authority = &rest[..authority_end];
    let remainder = &rest[authority_end..];

    // Check for `@` in the authority — indicates userinfo.
    if let Some(at_pos) = authority.rfind('@') {
        let host = &authority[at_pos + 1..];
        format!("{}://{}{}", &url[..scheme_end], host, remainder)
    } else {
        url.to_string()
    }
}

/// Sanitize a string that may contain embedded URLs with credentials.
///
/// Scans for common URI schemes and strips userinfo from each occurrence.
pub fn sanitize_embedded_urls(text: &str) -> String {
    // Common URI schemes that might carry credentials.
    const SCHEMES: &[&str] = &[
        "http://",
        "https://",
        "mongodb://",
        "mongodb+srv://",
        "postgresql://",
        "postgres://",
        "redis://",
        "rediss://",
        "amqp://",
        "amqps://",
        "mysql://",
        "ftp://",
        "sftp://",
        "ssh://",
        "nats://",
    ];

    let mut result = text.to_string();

    for scheme in SCHEMES {
        // Process all occurrences of each scheme.
        let mut search_from = 0;
        while let Some(idx) = result[search_from..].find(scheme) {
            let abs_idx = search_from + idx;
            // Extract the URL-like substring (up to whitespace or end).
            let url_start = abs_idx;
            let url_end = result[url_start..]
                .find(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == '>')
                .map(|e| url_start + e)
                .unwrap_or(result.len());

            let url = &result[url_start..url_end];
            let sanitized = strip_url_userinfo(url);

            if sanitized != url {
                result = format!(
                    "{}{}{}",
                    &result[..url_start],
                    sanitized,
                    &result[url_end..]
                );
            }

            search_from = url_start + sanitized.len();
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_mongodb_credentials() {
        assert_eq!(
            strip_url_userinfo("mongodb://admin:p4ssw0rd@db.example.com:27017/mydb"),
            "mongodb://db.example.com:27017/mydb"
        );
    }

    #[test]
    fn strip_https_credentials() {
        assert_eq!(
            strip_url_userinfo("https://user:token@api.github.com/repos"),
            "https://api.github.com/repos"
        );
    }

    #[test]
    fn no_credentials_unchanged() {
        assert_eq!(
            strip_url_userinfo("https://example.com/path?q=1"),
            "https://example.com/path?q=1"
        );
    }

    #[test]
    fn username_only_stripped() {
        assert_eq!(
            strip_url_userinfo("ftp://anonymous@files.example.com/pub"),
            "ftp://files.example.com/pub"
        );
    }

    #[test]
    fn no_scheme_unchanged() {
        assert_eq!(strip_url_userinfo("just-a-string"), "just-a-string");
    }

    #[test]
    fn complex_password_stripped() {
        assert_eq!(
            strip_url_userinfo("postgresql://user:p%40ss%3Aword@db:5432/app"),
            "postgresql://db:5432/app"
        );
    }

    #[test]
    fn sanitize_embedded_finds_urls_in_text() {
        let input = r#"connecting to mongodb://root:secret@mongo:27017/db and https://token:abc@api.example.com/v1"#;
        let output = sanitize_embedded_urls(input);
        assert!(!output.contains("root:secret"));
        assert!(!output.contains("token:abc"));
        assert!(output.contains("mongodb://mongo:27017/db"));
        assert!(output.contains("https://api.example.com/v1"));
    }

    #[test]
    fn sanitize_no_urls_unchanged() {
        let input = "no urls here, just text";
        assert_eq!(sanitize_embedded_urls(input), input);
    }
}
