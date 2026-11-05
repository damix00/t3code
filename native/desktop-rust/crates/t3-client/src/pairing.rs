//! Port of `packages/shared/src/remote.ts`: turning what a person pastes (a pairing URL, a
//! `t3code://pair` link, or a host plus pairing code) into backend URLs and a credential.

use url::Url;
use url::form_urlencoded;

const PAIRING_TOKEN_PARAM: &str = "token";
const HOSTED_PAIRING_HOST_PARAM: &str = "host";
const PAIRING_APP_LINK_PARAM: &str = "pairingUrl";
const SUPPORTED_PROTOCOLS: [&str; 4] = ["http", "https", "ws", "wss"];

/// Where a backend URL came from, for error reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendUrlSource {
    DirectHost,
    HostedPairingHost,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PairingError {
    #[error("Enter a backend URL.")]
    BackendUrlMissing,
    /// `protocol` is set (with its trailing colon, as in JavaScript) when the URL parsed but
    /// uses a scheme T3 Code cannot connect to.
    #[error("Pairing URL is invalid.")]
    PairingUrlInvalid { protocol: Option<String> },
    #[error("Backend URL is invalid.")]
    BackendUrlInvalid {
        origin: BackendUrlSource,
        protocol: Option<String>,
    },
    #[error("Pairing URL is missing its token.")]
    PairingTokenMissing { host: String },
    #[error("Enter a pairing code.")]
    PairingCodeMissing { host: String },
}

/// A backend to pair with and the one-time credential to exchange for an access token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingTarget {
    pub credential: String,
    pub http_base_url: String,
    pub ws_base_url: String,
}

/// What the person entered.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PairingInput {
    pub pairing_url: Option<String>,
    pub host: Option<String>,
    pub pairing_code: Option<String>,
}

/// JavaScript's `String.prototype.trim` whitespace: includes U+FEFF, excludes U+0085.
fn js_trim(text: &str) -> &str {
    text.trim_matches(|c: char| (c.is_whitespace() && c != '\u{85}') || c == '\u{feff}')
}

fn protocol(url: &Url) -> String {
    format!("{}:", url.scheme())
}

fn is_supported(url: &Url) -> bool {
    SUPPORTED_PROTOCOLS.contains(&url.scheme())
}

/// `url.host` in JavaScript: the host name plus a non-default port.
pub(crate) fn display_host(url: &Url) -> String {
    match (url.host_str(), url.port()) {
        (Some(host), Some(port)) => format!("{host}:{port}"),
        (Some(host), None) => host.to_owned(),
        (None, _) => String::new(),
    }
}

fn first_param(pairs: form_urlencoded::Parse<'_>, name: &str) -> Option<String> {
    pairs
        .into_iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.into_owned())
}

fn hash_param(url: &Url, name: &str) -> Option<String> {
    first_param(
        form_urlencoded::parse(url.fragment().unwrap_or("").as_bytes()),
        name,
    )
}

fn query_param(url: &Url, name: &str) -> Option<String> {
    first_param(url.query_pairs(), name)
}

fn non_empty_trimmed(value: Option<String>) -> Option<String> {
    value
        .map(|value| js_trim(&value).to_owned())
        .filter(|value| !value.is_empty())
}

/// The pairing token from the URL fragment, falling back to the query string.
pub fn pairing_token_from_url(url: &Url) -> Option<String> {
    non_empty_trimmed(hash_param(url, PAIRING_TOKEN_PARAM))
        .or_else(|| non_empty_trimmed(query_param(url, PAIRING_TOKEN_PARAM)))
}

/// Whether `text` begins with `scheme://`, where a scheme is a letter followed by letters,
/// digits, `+` or `-`.
fn has_scheme_prefix(text: &str) -> bool {
    let Some((scheme, _)) = text.split_once("://") else {
        return false;
    };
    let mut chars = scheme.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '-')
}

fn normalize_remote_base_url(raw: &str, source: BackendUrlSource) -> Result<Url, PairingError> {
    let trimmed = js_trim(raw);
    if trimmed.is_empty() {
        return Err(PairingError::BackendUrlMissing);
    }
    let without_slashes = trimmed.trim_start_matches('/');
    let input = if has_scheme_prefix(without_slashes) {
        without_slashes.to_owned()
    } else {
        format!("https://{without_slashes}")
    };
    let mut url = Url::parse(&input).map_err(|_| PairingError::BackendUrlInvalid {
        origin: source,
        protocol: None,
    })?;
    if !is_supported(&url) {
        return Err(PairingError::BackendUrlInvalid {
            origin: source,
            protocol: Some(protocol(&url)),
        });
    }
    clear_path(&mut url);
    Ok(url)
}

fn clear_path(url: &mut Url) {
    url.set_path("/");
    url.set_query(None);
    url.set_fragment(None);
}

fn with_scheme(url: &Url, from: &str, to: &str) -> String {
    let mut next = url.clone();
    if next.scheme() == from {
        // Switching between special schemes always succeeds.
        let _ = next.set_scheme(to);
    }
    clear_path(&mut next);
    next.to_string()
}

/// The HTTP origin of a backend URL, with `ws:`/`wss:` mapped to `http:`/`https:`.
pub fn to_http_base_url(url: &Url) -> String {
    let next = with_scheme(url, "ws", "http");
    Url::parse(&next)
        .map(|url| with_scheme(&url, "wss", "https"))
        .unwrap_or(next)
}

/// The WebSocket origin of a backend URL, with `http:`/`https:` mapped to `ws:`/`wss:`.
pub fn to_ws_base_url(url: &Url) -> String {
    let next = with_scheme(url, "http", "ws");
    Url::parse(&next)
        .map(|url| with_scheme(&url, "https", "wss"))
        .unwrap_or(next)
}

fn target(credential: String, url: &Url) -> PairingTarget {
    PairingTarget {
        credential,
        http_base_url: to_http_base_url(url),
        ws_base_url: to_ws_base_url(url),
    }
}

/// `resolveRemotePairingTarget`. A pairing URL wins over a host and code.
pub fn resolve_pairing_target(input: &PairingInput) -> Result<PairingTarget, PairingError> {
    let pairing_url = js_trim(input.pairing_url.as_deref().unwrap_or(""));
    if !pairing_url.is_empty() {
        let url = Url::parse(pairing_url)
            .map_err(|_| PairingError::PairingUrlInvalid { protocol: None })?;
        if !is_supported(&url) {
            return Err(PairingError::PairingUrlInvalid {
                protocol: Some(protocol(&url)),
            });
        }
        let hosted_host = non_empty_trimmed(query_param(&url, HOSTED_PAIRING_HOST_PARAM));
        let token = pairing_token_from_url(&url);
        if let (Some(host), Some(token)) = (hosted_host, token.clone()) {
            let backend = normalize_remote_base_url(&host, BackendUrlSource::HostedPairingHost)?;
            return Ok(target(token, &backend));
        }
        let credential = token.ok_or_else(|| PairingError::PairingTokenMissing {
            host: display_host(&url),
        })?;
        return Ok(target(credential, &url));
    }

    let host = js_trim(input.host.as_deref().unwrap_or(""));
    let code = js_trim(input.pairing_code.as_deref().unwrap_or(""));
    if host.is_empty() {
        return Err(PairingError::BackendUrlMissing);
    }
    let backend = normalize_remote_base_url(host, BackendUrlSource::DirectHost)?;
    if code.is_empty() {
        return Err(PairingError::PairingCodeMissing {
            host: display_host(&backend),
        });
    }
    Ok(target(code.to_owned(), &backend))
}

/// Wraps a pairing URL in the `t3code://pair` link that desktop opens and mobile scans.
pub fn build_pairing_app_link(pairing_url: &str) -> String {
    let query = form_urlencoded::Serializer::new(String::new())
        .append_pair(PAIRING_APP_LINK_PARAM, pairing_url)
        .finish();
    format!("t3code://pair?{query}")
}

fn is_app_link_scheme(scheme: &str) -> bool {
    match scheme.strip_prefix("t3code") {
        Some("") => true,
        Some(rest) => rest.strip_prefix('-').is_some_and(|channel| {
            !channel.is_empty() && channel.chars().all(|c| c.is_ascii_lowercase())
        }),
        None => false,
    }
}

/// Reads the pairing URL from a `t3code://pair` link on any app channel scheme
/// (`t3code:`, `t3code-dev:`, …).
pub fn read_pairing_app_link(value: &str) -> Option<String> {
    let url = Url::parse(value).ok()?;
    if !is_app_link_scheme(url.scheme()) || url.host_str() != Some("pair") || url.port().is_some() {
        return None;
    }
    if !url.path().is_empty() && url.path() != "/" {
        return None;
    }
    non_empty_trimmed(query_param(&url, PAIRING_APP_LINK_PARAM))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn from_url(pairing_url: &str) -> Result<PairingTarget, PairingError> {
        resolve_pairing_target(&PairingInput {
            pairing_url: Some(pairing_url.to_owned()),
            ..Default::default()
        })
    }

    fn from_host(host: &str, code: &str) -> Result<PairingTarget, PairingError> {
        resolve_pairing_target(&PairingInput {
            host: Some(host.to_owned()),
            pairing_code: Some(code.to_owned()),
            ..Default::default()
        })
    }

    fn expected(credential: &str, http: &str, ws: &str) -> PairingTarget {
        PairingTarget {
            credential: credential.to_owned(),
            http_base_url: http.to_owned(),
            ws_base_url: ws.to_owned(),
        }
    }

    fn remote() -> PairingTarget {
        expected(
            "pairing-token",
            "https://remote.example.com/",
            "wss://remote.example.com/",
        )
    }

    #[test]
    fn derives_backend_urls_and_token_from_a_pairing_url() {
        assert_eq!(
            from_url("https://remote.example.com/pair#token=pairing-token"),
            Ok(remote())
        );
    }

    #[test]
    fn accepts_pairing_urls_that_still_use_a_query_token() {
        assert_eq!(
            from_url("https://remote.example.com/pair?token=pairing-token"),
            Ok(remote())
        );
    }

    #[test]
    fn fragment_token_wins_over_query_token() {
        let target = from_url("https://remote.example.com/pair?token=query#token=hash").unwrap();
        assert_eq!(target.credential, "hash");
        let target = from_url("https://remote.example.com/pair?token=query#token=%20%20").unwrap();
        assert_eq!(target.credential, "query");
    }

    #[test]
    fn derives_backend_urls_from_hosted_app_pairing_links() {
        assert_eq!(
            from_url(
                "https://app.t3.codes/pair?host=https%3A%2F%2Fdesktop.tailnet.ts.net%3A44342%2F#token=pairing-token"
            ),
            Ok(expected(
                "pairing-token",
                "https://desktop.tailnet.ts.net:44342/",
                "wss://desktop.tailnet.ts.net:44342/"
            ))
        );
    }

    #[test]
    fn hosted_link_with_an_invalid_host_reports_its_source() {
        assert_eq!(
            from_url("https://app.t3.codes/pair?host=ftp%3A%2F%2Fbox#token=t"),
            Err(PairingError::BackendUrlInvalid {
                origin: BackendUrlSource::HostedPairingHost,
                protocol: Some("ftp:".into())
            })
        );
    }

    #[test]
    fn derives_backend_urls_from_a_host_and_pairing_code() {
        assert_eq!(
            from_host("https://remote.example.com", "pairing-token"),
            Ok(remote())
        );
        assert_eq!(
            from_host("  remote.example.com  ", "  pairing-token "),
            Ok(remote())
        );
    }

    #[test]
    fn treats_a_protocol_relative_host_as_https() {
        assert_eq!(
            from_host("//remote.example.com", "pairing-token"),
            Ok(remote())
        );
    }

    #[test]
    fn collapses_extra_leading_slashes_instead_of_producing_an_empty_host() {
        assert_eq!(
            from_host("///example.com", "pairing-token"),
            Ok(expected(
                "pairing-token",
                "https://example.com/",
                "wss://example.com/"
            ))
        );
    }

    #[test]
    fn does_not_double_prepend_https_when_the_host_already_carries_a_scheme() {
        assert_eq!(
            from_host("//https://example.com", "pairing-token"),
            Ok(expected(
                "pairing-token",
                "https://example.com/",
                "wss://example.com/"
            ))
        );
    }

    #[test]
    fn preserves_host_ports_when_normalizing_a_bare_host_input() {
        assert_eq!(
            from_host("myserver.com:3000", "pairing-token"),
            Ok(expected(
                "pairing-token",
                "https://myserver.com:3000/",
                "wss://myserver.com:3000/"
            ))
        );
    }

    #[test]
    fn maps_websocket_and_plain_http_schemes_both_ways() {
        assert_eq!(
            from_host("ws://10.0.0.2:3773/ws?x=1#y", "c"),
            Ok(expected(
                "c",
                "http://10.0.0.2:3773/",
                "ws://10.0.0.2:3773/"
            ))
        );
        assert_eq!(
            from_host("wss://box.example", "c"),
            Ok(expected("c", "https://box.example/", "wss://box.example/"))
        );
        assert_eq!(
            from_host("http://localhost:3773", "c"),
            Ok(expected(
                "c",
                "http://localhost:3773/",
                "ws://localhost:3773/"
            ))
        );
    }

    #[test]
    fn rejects_unsupported_direct_pairing_url_protocols() {
        assert_eq!(
            from_url("ftp://remote.example.com/pair#token=pairing-token"),
            Err(PairingError::PairingUrlInvalid {
                protocol: Some("ftp:".into())
            })
        );
        assert_eq!(
            from_url("not a url"),
            Err(PairingError::PairingUrlInvalid { protocol: None })
        );
    }

    #[test]
    fn rejects_unsupported_host_protocols() {
        assert_eq!(
            from_host("ftp://box", "code"),
            Err(PairingError::BackendUrlInvalid {
                origin: BackendUrlSource::DirectHost,
                protocol: Some("ftp:".into())
            })
        );
    }

    #[test]
    fn uses_distinct_structural_errors_for_missing_pairing_inputs() {
        assert_eq!(
            resolve_pairing_target(&PairingInput::default()),
            Err(PairingError::BackendUrlMissing)
        );
        assert_eq!(
            from_url("https://remote.example.com/pair"),
            Err(PairingError::PairingTokenMissing {
                host: "remote.example.com".into()
            })
        );
        assert_eq!(
            resolve_pairing_target(&PairingInput {
                host: Some(
                    "https://user:secret@remote.example.com/path?token=sensitive#fragment".into()
                ),
                ..Default::default()
            }),
            Err(PairingError::PairingCodeMissing {
                host: "remote.example.com".into()
            })
        );
        assert_eq!(
            from_host("myserver.com:3000", "   "),
            Err(PairingError::PairingCodeMissing {
                host: "myserver.com:3000".into()
            })
        );
    }

    #[test]
    fn blank_pairing_url_falls_back_to_host_and_code() {
        let input = PairingInput {
            pairing_url: Some("   ".into()),
            host: Some("remote.example.com".into()),
            pairing_code: Some("pairing-token".into()),
        };
        assert_eq!(resolve_pairing_target(&input), Ok(remote()));
    }

    #[test]
    fn round_trips_pairing_urls_through_app_links_on_every_channel_scheme() {
        let pairing_url = "https://vps.example.com/pair#token=ABC123";
        let app_link = build_pairing_app_link(pairing_url);
        assert!(app_link.starts_with("t3code://pair?"));
        assert_eq!(
            read_pairing_app_link(&app_link).as_deref(),
            Some(pairing_url)
        );
        assert_eq!(
            read_pairing_app_link(&app_link.replace("t3code:", "t3code-dev:")).as_deref(),
            Some(pairing_url)
        );
        assert_eq!(read_pairing_app_link("t3code://pair"), None);
        assert_eq!(
            read_pairing_app_link(&app_link.replace("//pair?", "//pair/other?")),
            None
        );
        let encoded: String = form_urlencoded::byte_serialize(pairing_url.as_bytes()).collect();
        assert_eq!(
            read_pairing_app_link(&format!("t3code://threads?pairingUrl={encoded}")),
            None
        );
        assert_eq!(
            read_pairing_app_link(&format!("https://pair?pairingUrl={encoded}")),
            None
        );
        assert_eq!(
            read_pairing_app_link(&format!("t3code-DEV1://pair?pairingUrl={encoded}")),
            None
        );
        assert_eq!(
            read_pairing_app_link(&format!("t3code-://pair?pairingUrl={encoded}")),
            None
        );
        assert_eq!(read_pairing_app_link("not a link"), None);
    }

    #[test]
    fn app_link_encodes_like_url_search_params() {
        assert_eq!(
            build_pairing_app_link("https://a.b/pair#token=x y&z"),
            "t3code://pair?pairingUrl=https%3A%2F%2Fa.b%2Fpair%23token%3Dx+y%26z"
        );
    }

    #[test]
    fn js_trim_matches_javascript() {
        assert_eq!(js_trim("\u{feff} a \n"), "a");
        assert_eq!(js_trim("\u{85}a"), "\u{85}a");
    }
}
