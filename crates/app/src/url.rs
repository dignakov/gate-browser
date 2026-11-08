//! What the address bar means: addresses vs searches, and how they show.

/// What new tabs and panes open.
pub const START: &str = "about:blank";

const SEARCH: &str = "https://www.google.com/search?q=";

/// Turns what someone typed into a URL to load. A leading `?` searches
/// whatever follows, even if it looks like an address.
pub fn resolve(input: &str) -> String {
    let text = input.trim();
    if text.is_empty() {
        return START.to_string();
    }
    if let Some(words) = text.strip_prefix('?') {
        return search(words);
    }
    if has_scheme(text) {
        return text.to_string();
    }
    if !text.contains(char::is_whitespace) && looks_like_host(text) {
        let local = is_local(text);
        return format!("{}://{text}", if local { "http" } else { "https" });
    }
    search(text)
}

/// The web search for `words`.
pub fn search(words: &str) -> String {
    format!("{SEARCH}{}", encode(words.trim_start_matches('?').trim()))
}

/// Whether typed text means an address (not a search).
pub fn is_address(input: &str) -> bool {
    let text = input.trim();
    has_scheme(text) || (!text.is_empty() && !text.contains(char::is_whitespace) && looks_like_host(text))
}

/// How a URL shows in the address bar: blank pages show nothing.
pub fn display(url: &str) -> String {
    if url == START || url.is_empty() { String::new() } else { url.to_string() }
}

/// The host part, for compact labels.
pub fn host(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let host = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    host.strip_prefix("www.").unwrap_or(host)
}

fn has_scheme(text: &str) -> bool {
    if text.contains("://") {
        return true;
    }
    ["about:", "data:", "file:", "chrome:", "javascript:", "view-source:", "mailto:"]
        .iter()
        .any(|s| text.starts_with(s))
}

fn looks_like_host(text: &str) -> bool {
    let host = text.split(['/', '?', '#']).next().unwrap_or(text);
    let name = host.rsplit_once(':').map_or(host, |(h, port)| {
        if port.chars().all(|c| c.is_ascii_digit()) { h } else { host }
    });
    if name == "localhost" || name.parse::<std::net::Ipv4Addr>().is_ok() {
        return true;
    }
    // A dot with a plausible top-level label: "example.com", not "e.g".
    name.rsplit_once('.')
        .is_some_and(|(left, tld)| !left.is_empty() && tld.len() >= 2 && tld.chars().all(|c| c.is_ascii_alphanumeric()))
}

fn is_local(text: &str) -> bool {
    let host = text.split(['/', ':']).next().unwrap_or(text);
    host == "localhost" || host.starts_with("127.") || host.starts_with("192.168.") || host.starts_with("10.")
}

fn encode(text: &str) -> String {
    let mut out = String::new();
    for b in text.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves() {
        assert_eq!(resolve("example.com"), "https://example.com");
        assert_eq!(resolve("localhost:3000/x"), "http://localhost:3000/x");
        assert_eq!(resolve("127.0.0.1:8787"), "http://127.0.0.1:8787");
        assert_eq!(resolve("https://a.b/c"), "https://a.b/c");
        assert_eq!(resolve("rust gpui"), "https://www.google.com/search?q=rust+gpui");
        assert_eq!(resolve("e.g"), "https://www.google.com/search?q=e.g");
        assert_eq!(resolve("about:blank"), "about:blank");
        assert_eq!(resolve("c++"), "https://www.google.com/search?q=c%2B%2B");
        // A leading `?` searches, even what looks like an address.
        assert_eq!(resolve("?example.com"), "https://www.google.com/search?q=example.com");
        assert_eq!(resolve("? rust gpui"), "https://www.google.com/search?q=rust+gpui");
    }

    #[test]
    fn hosts() {
        assert_eq!(host("https://www.herdr.dev/docs"), "herdr.dev");
        assert_eq!(host("about:blank"), "about:blank");
    }
}
