//! A bearer token on the pairing URL.
//!
//! # What this does not give you
//!
//! Read this part first, because the failure mode of an auth mechanism is
//! someone downstream believing it covers more than it does.
//!
//! - **It is not confidentiality.** The token travels in a URL over plain
//!   HTTP. Anyone who can watch the network — another device on the Wi-Fi, a
//!   compromised router — reads it in cleartext and is then as authorized as
//!   the phone. Closing that requires TLS, which is separate work.
//! - **It is not identity.** Everyone who has the token is the same principal.
//!   There is no way to revoke one phone without revoking all of them.
//! - **It is not a defence against anyone with filesystem access.** The token
//!   is stored in the settings file in plaintext, next to the log.
//!
//! # What it does give you
//!
//! One thing, and it is the thing that is actually exploitable today:
//!
//! **A web page you visit cannot drive your VR player.** Without a token, any
//! page in any tab could `fetch('http://192.168.0.9:8787/healthz')` and read
//! the media URL and LAN address, and — worse — open a WebSocket to the bridge
//! and issue `seek`, `play` and `pause`. WebSockets are not subject to the
//! same-origin policy, so no CORS setting prevents it. The attacker does not
//! need to be on the network; they need the victim to load a page. Guessing a
//! 256-bit secret is not a viable substitute for being handed one.
//!
//! So: the token stops the drive-by. TLS stops the eavesdropper. **Neither
//! makes the other unnecessary**, and this file is not evidence that "the
//! bridge is authenticated" in any broader sense.
//!
//! # Why it persists rather than regenerating each run
//!
//! The point of the pairing flow is that the phone installs the app to its
//! home screen and opens it later without ceremony. A per-run token would put
//! a fresh secret in the URL on every restart, so every restart would break
//! that shortcut — the same class of problem as a churning hostname, which is
//! precisely what the TLS work exists to eliminate. A stable origin with an
//! unstable credential is not a stable origin.
//!
//! So the token lives in the app's settings file and survives restarts, and
//! rotation is an explicit action rather than a side effect of a restart.

use serde::{Deserialize, Serialize};

/// 32 bytes, hex-encoded. Long enough that online guessing is not a strategy,
/// short enough to sit in a QR code without pushing it to a denser version.
const TOKEN_BYTES: usize = 32;

/// A shared secret carried on the pairing URL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Token(String);

impl Token {
    /// Mint a new token from the operating system's RNG.
    ///
    /// `getrandom` rather than anything hand-rolled: it is a thin wrapper over
    /// the platform CSPRNG. Deriving key material from timestamps, process ids
    /// or `RandomState` is the classic way to produce a secret with far less
    /// entropy than its length suggests, and saving one small dependency is
    /// not a reason to take that risk.
    pub fn generate() -> Self {
        let mut bytes = [0u8; TOKEN_BYTES];
        getrandom::getrandom(&mut bytes).expect("the OS RNG must be available");
        let mut hex = String::with_capacity(TOKEN_BYTES * 2);
        for byte in bytes {
            use std::fmt::Write;
            let _ = write!(hex, "{byte:02x}");
        }
        Self(hex)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Adopt a token supplied from outside — a settings file, or `--token`.
    ///
    /// Deliberately not validating the shape. A caller pinning a token has
    /// chosen its entropy, and rejecting anything that is not 64 hex
    /// characters would be enforcing our format on their secret while doing
    /// nothing to enforce its strength.
    pub fn from_string(raw: String) -> Self {
        Self(raw)
    }

    /// Compare without leaking where the mismatch was.
    ///
    /// A `==` on `String` short-circuits at the first differing byte, which in
    /// principle lets an attacker who can measure the difference recover the
    /// token one byte at a time. Over a LAN with a 1 Hz protocol that attack is
    /// not realistic — but constant-time comparison costs four lines, and
    /// "probably not exploitable" is a poor thing to have to re-evaluate later.
    pub fn matches(&self, candidate: &str) -> bool {
        let expected = self.0.as_bytes();
        let actual = candidate.as_bytes();
        if expected.len() != actual.len() {
            return false;
        }
        let mut difference = 0u8;
        for (a, b) in expected.iter().zip(actual) {
            difference |= a ^ b;
        }
        difference == 0
    }
}

impl std::fmt::Display for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The query-string key the token travels under.
pub const TOKEN_PARAM: &str = "t";

/// Pull `?t=…` out of a request target or a URL.
pub fn token_from_query(path_and_query: &str) -> Option<&str> {
    let (_, query) = path_and_query.split_once('?')?;
    query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key == TOKEN_PARAM).then_some(value)
    })
}

/// Append the token to a URL that may already carry a query string.
pub fn with_token(url: &str, token: &Token) -> String {
    let separator = if url.contains('?') { '&' } else { '?' };
    format!("{url}{separator}{TOKEN_PARAM}={token}")
}

/// Whether a WebSocket upgrade's `Origin` is one we are willing to serve.
///
/// Browsers set `Origin` on WebSocket handshakes and forbid pages from
/// overriding it, so it is a usable signal *against a browser* — which is the
/// attacker this defends against. It is trivially forged by a non-browser
/// client, and so is a second lock on the same door as the token rather than
/// an independent one.
///
/// A missing `Origin` is allowed: native clients (the desktop app, a script,
/// `websocat`) do not send one, and they still have to present a token.
pub fn origin_is_acceptable(origin: Option<&str>, allowed_hosts: &[String]) -> bool {
    let Some(origin) = origin else {
        return true;
    };
    let origin = origin.trim();
    if origin.eq_ignore_ascii_case("null") {
        // Sandboxed iframes and `file://` pages. Nothing legitimate reaches us
        // that way, and it is a common carrier for drive-by requests.
        return false;
    }
    let host = origin
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(origin);
    allowed_hosts
        .iter()
        .any(|allowed| host.eq_ignore_ascii_case(allowed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_long_and_not_repeated() {
        let a = Token::generate();
        let b = Token::generate();
        assert_eq!(a.as_str().len(), TOKEN_BYTES * 2);
        assert!(a.as_str().chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b, "two tokens from the OS RNG must not collide");
    }

    #[test]
    fn matching_is_exact() {
        let token = Token::generate();
        assert!(token.matches(token.as_str()));
        assert!(!token.matches(""));
        assert!(!token.matches(&token.as_str()[..10]));
        assert!(!token.matches(&format!("{token}x")));

        let mut wrong = token.as_str().to_string();
        // Flip the last character — the case a short-circuiting compare would
        // take longest to reject.
        wrong.pop();
        wrong.push(if token.as_str().ends_with('a') { 'b' } else { 'a' });
        assert!(!token.matches(&wrong));
    }

    #[test]
    fn tokens_are_read_out_of_a_query_string() {
        assert_eq!(token_from_query("/ws?t=abc"), Some("abc"));
        assert_eq!(token_from_query("/healthz?pretty=1&t=abc"), Some("abc"));
        assert_eq!(token_from_query("/ws?t=abc&other=1"), Some("abc"));
        assert_eq!(token_from_query("/ws"), None);
        assert_eq!(token_from_query("/ws?other=1"), None);
        // A key that merely ends in `t` is not the token.
        assert_eq!(token_from_query("/ws?nott=abc"), None);
    }

    #[test]
    fn urls_gain_the_token_without_breaking_an_existing_query() {
        let token = Token::generate();
        let plain = with_token("http://host:8787/", &token);
        assert!(plain.contains(&format!("?t={token}")));
        let existing = with_token("http://host:8787/?a=1", &token);
        assert!(existing.contains(&format!("&t={token}")));
    }

    #[test]
    fn origins_outside_our_own_hosts_are_refused() {
        let allowed = vec!["192.168.0.9:8787".to_string(), "127.0.0.1:8787".to_string()];
        assert!(origin_is_acceptable(Some("http://192.168.0.9:8787"), &allowed));
        assert!(origin_is_acceptable(Some("https://127.0.0.1:8787"), &allowed));
        // The attacker this exists for.
        assert!(!origin_is_acceptable(Some("https://evil.example"), &allowed));
        assert!(!origin_is_acceptable(Some("null"), &allowed));
        // A native client sends no Origin and is judged on its token alone.
        assert!(origin_is_acceptable(None, &allowed));
    }
}
