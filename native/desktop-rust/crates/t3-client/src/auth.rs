//! Port of `packages/client-runtime/src/authorization/remote.ts`: exchanging a bootstrap
//! credential for a bearer token, and minting the single-use ticket a WebSocket connects with.

use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;
use url::Url;

use crate::pairing::display_host;

pub const TOKEN_EXCHANGE_GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:token-exchange";
pub const ACCESS_TOKEN_TYPE: &str = "urn:ietf:params:oauth:token-type:access_token";
pub const ENVIRONMENT_BOOTSTRAP_TOKEN_TYPE: &str =
    "urn:t3:params:oauth:token-type:environment-bootstrap";

const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const TRANSIENT_STATUS_CODES: [u16; 3] = [502, 503, 504];

/// Who is connecting, as the environment shows it in its list of clients.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClientMetadata {
    pub label: Option<String>,
    /// `desktop`, `mobile`, `tablet`, `bot` or `unknown`.
    pub device_type: Option<String>,
    pub os: Option<String>,
    /// `web`, `desktop`, `mobile` or `cli`.
    pub surface: Option<String>,
    pub app_version: Option<String>,
}

impl ClientMetadata {
    /// The metadata this app sends.
    pub fn desktop() -> Self {
        Self {
            label: Some("T3 Code Desktop".into()),
            device_type: Some("desktop".into()),
            os: Some(current_os().into()),
            surface: Some("desktop".into()),
            app_version: Some(env!("CARGO_PKG_VERSION").into()),
        }
    }
}

/// The OS names upstream reports.
pub fn current_os() -> &'static str {
    match std::env::consts::OS {
        "macos" => "macos",
        "windows" => "windows",
        "linux" => "linux",
        _ => "other",
    }
}

/// A bearer token from `POST /oauth/token`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct AccessToken {
    pub access_token: String,
    #[serde(default)]
    pub issued_token_type: Option<String>,
    pub token_type: String,
    #[serde(default)]
    pub expires_in: Option<f64>,
    #[serde(default)]
    pub scope: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebSocketTicket {
    pub ticket: String,
    #[serde(default)]
    pub expires_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum AuthError {
    /// A declared error from the environment, e.g. `EnvironmentAuthInvalidError`.
    #[error("{}", describe_environment_error(.tag, .reason.as_deref()))]
    Environment {
        status: u16,
        tag: String,
        code: Option<String>,
        reason: Option<String>,
    },
    #[error("Remote environment endpoint {url} returned undeclared status {status}.")]
    UndeclaredStatus { url: String, status: u16 },
    #[error("Remote environment endpoint {url} timed out after {}ms.", .timeout.as_millis())]
    Timeout { url: String, timeout: Duration },
    #[error("Could not reach {url}: {message}")]
    Fetch { url: String, message: String },
    #[error("Remote environment endpoint {url} returned invalid JSON.")]
    InvalidJson { url: String },
}

fn describe_environment_error(tag: &str, reason: Option<&str>) -> String {
    let what = match tag {
        "EnvironmentAuthInvalidError" => "The environment rejected the credential",
        "EnvironmentRequestInvalidError" => "The environment rejected the request",
        "EnvironmentScopeRequiredError" => "The environment requires more access",
        "EnvironmentOperationForbiddenError" => "The environment forbids this operation",
        "EnvironmentResourceNotFoundError" => "The environment could not find the resource",
        "EnvironmentInternalError" => "The environment hit an internal error",
        other => other,
    };
    match reason {
        Some(reason) => format!("{what} ({reason})."),
        None => format!("{what}."),
    }
}

impl AuthError {
    /// Whether retrying shortly may succeed: the server is still starting or a proxy hiccuped.
    pub fn is_transient(&self) -> bool {
        match self {
            Self::Fetch { .. } | Self::Timeout { .. } => true,
            Self::UndeclaredStatus { status, .. } => TRANSIENT_STATUS_CODES.contains(status),
            Self::Environment { .. } | Self::InvalidJson { .. } => false,
        }
    }

    /// Whether the credential itself was refused, so asking again will not help.
    pub fn is_unauthorized(&self) -> bool {
        matches!(self, Self::Environment { status: 401, .. })
    }
}

/// `environmentEndpointUrl`: the base URL with `pathname` and nothing after it.
pub fn endpoint_url(http_base_url: &str, pathname: &str) -> Result<Url, AuthError> {
    let mut url = Url::parse(http_base_url).map_err(|error| AuthError::Fetch {
        url: http_base_url.to_owned(),
        message: error.to_string(),
    })?;
    url.set_path(pathname);
    url.set_query(None);
    url.set_fragment(None);
    Ok(url)
}

/// Talks to an environment's HTTP auth endpoints.
#[derive(Debug, Clone)]
pub struct AuthClient {
    http: reqwest::Client,
    timeout: Duration,
}

impl Default for AuthClient {
    fn default() -> Self {
        Self::new(reqwest::Client::new())
    }
}

impl AuthClient {
    pub fn new(http: reqwest::Client) -> Self {
        Self {
            http,
            timeout: DEFAULT_REQUEST_TIMEOUT,
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    async fn execute<T: serde::de::DeserializeOwned>(
        &self,
        url: &Url,
        request: reqwest::RequestBuilder,
    ) -> Result<T, AuthError> {
        let url_text = url.to_string();
        let response = match tokio::time::timeout(self.timeout, request.send()).await {
            Err(_) => {
                return Err(AuthError::Timeout {
                    url: url_text,
                    timeout: self.timeout,
                });
            }
            Ok(Err(error)) if error.is_timeout() => {
                return Err(AuthError::Timeout {
                    url: url_text,
                    timeout: self.timeout,
                });
            }
            Ok(Err(error)) => {
                return Err(AuthError::Fetch {
                    url: url_text,
                    message: error.to_string(),
                });
            }
            Ok(Ok(response)) => response,
        };
        let status = response.status().as_u16();
        let body = match tokio::time::timeout(self.timeout, response.bytes()).await {
            Err(_) => {
                return Err(AuthError::Timeout {
                    url: url_text,
                    timeout: self.timeout,
                });
            }
            Ok(Err(error)) => {
                return Err(AuthError::Fetch {
                    url: url_text,
                    message: error.to_string(),
                });
            }
            Ok(Ok(body)) => body,
        };
        if (200..300).contains(&status) {
            return serde_json::from_slice(&body)
                .map_err(|_| AuthError::InvalidJson { url: url_text });
        }
        Err(classify_failure(url_text, status, &body))
    }

    /// `bootstrapRemoteBearerSession`: exchanges a pairing or desktop bootstrap credential for an
    /// access token.
    pub async fn exchange_bootstrap_credential(
        &self,
        http_base_url: &str,
        credential: &str,
        metadata: &ClientMetadata,
    ) -> Result<AccessToken, AuthError> {
        let url = endpoint_url(http_base_url, "/oauth/token")?;
        let form = token_exchange_form(credential, None, metadata);
        let request = self.http.post(url.clone()).form(&form);
        self.execute(&url, request).await
    }

    /// Like [`Self::exchange_bootstrap_credential`], retrying transient failures every 500ms for
    /// up to `budget`, as the desktop shell does while its local backend starts.
    pub async fn exchange_with_retry(
        &self,
        http_base_url: &str,
        credential: &str,
        metadata: &ClientMetadata,
        budget: Duration,
    ) -> Result<AccessToken, AuthError> {
        const INTERVAL: Duration = Duration::from_millis(500);
        let deadline = tokio::time::Instant::now() + budget;
        loop {
            match self
                .exchange_bootstrap_credential(http_base_url, credential, metadata)
                .await
            {
                Err(error)
                    if error.is_transient()
                        && tokio::time::Instant::now() + INTERVAL < deadline =>
                {
                    tracing::debug!(%error, "token exchange failed; retrying");
                    tokio::time::sleep(INTERVAL).await;
                }
                result => return result,
            }
        }
    }

    /// `issueRemoteWebSocketTicket` for the `rpc` use. Servers that predate ticket audiences
    /// answer the `/rpc` route with 404; the bare route issues the unbound ticket they expect.
    pub async fn issue_rpc_ticket(
        &self,
        http_base_url: &str,
        bearer_token: &str,
    ) -> Result<WebSocketTicket, AuthError> {
        match self
            .issue_ticket(
                http_base_url,
                bearer_token,
                "/api/auth/websocket-ticket/rpc",
            )
            .await
        {
            Err(AuthError::UndeclaredStatus { status: 404, .. })
            | Err(AuthError::Environment { status: 404, .. }) => {
                self.issue_ticket(http_base_url, bearer_token, "/api/auth/websocket-ticket")
                    .await
            }
            result => result,
        }
    }

    async fn issue_ticket(
        &self,
        http_base_url: &str,
        bearer_token: &str,
        path: &str,
    ) -> Result<WebSocketTicket, AuthError> {
        let url = endpoint_url(http_base_url, path)?;
        let request = self.http.post(url.clone()).bearer_auth(bearer_token);
        self.execute(&url, request).await
    }

    /// Reads the environment descriptor, which needs no authorization.
    pub async fn environment_descriptor(
        &self,
        http_base_url: &str,
    ) -> Result<EnvironmentDescriptor, AuthError> {
        let url = endpoint_url(http_base_url, ENVIRONMENT_DESCRIPTOR_PATH)?;
        let request = self.http.get(url.clone());
        self.execute(&url, request).await
    }

    /// `resolveRemoteWebSocketConnectionUrl`: a fresh ticket baked into the `/ws` URL.
    pub async fn websocket_connection_url(
        &self,
        http_base_url: &str,
        ws_base_url: &str,
        bearer_token: &str,
        metadata: &ClientMetadata,
    ) -> Result<String, AuthError> {
        let ticket = self.issue_rpc_ticket(http_base_url, bearer_token).await?;
        rpc_socket_url(ws_base_url, &ticket.ticket, metadata).map_err(|message| AuthError::Fetch {
            url: ws_base_url.to_owned(),
            message,
        })
    }
}

fn classify_failure(url: String, status: u16, body: &[u8]) -> AuthError {
    let parsed: Option<Value> = serde_json::from_slice(body).ok();
    let tag = parsed
        .as_ref()
        .and_then(|value| value.get("_tag"))
        .and_then(Value::as_str)
        .filter(|tag| tag.starts_with("Environment"));
    match (tag, parsed.as_ref()) {
        (Some(tag), Some(value)) => AuthError::Environment {
            status,
            tag: tag.to_owned(),
            code: value.get("code").and_then(Value::as_str).map(str::to_owned),
            reason: value
                .get("reason")
                .and_then(Value::as_str)
                .map(str::to_owned),
        },
        _ => AuthError::UndeclaredStatus { url, status },
    }
}

/// The form body of a token exchange, in upstream's field order.
pub fn token_exchange_form(
    credential: &str,
    scope: Option<&str>,
    metadata: &ClientMetadata,
) -> Vec<(&'static str, String)> {
    let mut form = vec![
        ("grant_type", TOKEN_EXCHANGE_GRANT_TYPE.to_owned()),
        ("subject_token", credential.to_owned()),
        (
            "subject_token_type",
            ENVIRONMENT_BOOTSTRAP_TOKEN_TYPE.to_owned(),
        ),
        ("requested_token_type", ACCESS_TOKEN_TYPE.to_owned()),
    ];
    if let Some(scope) = scope {
        form.push(("scope", scope.to_owned()));
    }
    if let Some(label) = &metadata.label {
        form.push(("client_label", label.clone()));
    }
    if let Some(device_type) = &metadata.device_type {
        form.push(("client_device_type", device_type.clone()));
    }
    if let Some(os) = metadata
        .os
        .as_deref()
        .filter(|os| *os != "unknown" && *os != "other")
    {
        form.push(("client_os", os.to_owned()));
    }
    form
}

/// `appendClientConnectionParams` for the fields a desktop client has.
pub fn append_client_connection_params(url: &mut Url, metadata: &ClientMetadata) {
    let mut pairs = url.query_pairs_mut();
    if let Some(surface) = &metadata.surface {
        pairs.append_pair("clientSurface", surface);
    }
    if let Some(version) = &metadata.app_version {
        pairs.append_pair("clientAppVersion", version);
    }
    if let Some(device_type) = &metadata.device_type {
        let device_type = match device_type.as_str() {
            "mobile" => "phone",
            "desktop" | "tablet" => device_type,
            _ => "unknown",
        };
        pairs.append_pair("clientDeviceType", device_type);
    }
    if let Some(os) = &metadata.os {
        pairs.append_pair("clientOs", os);
    }
}

/// The RPC socket URL: `/ws` when the base has no path, plus the ticket and client details.
pub fn rpc_socket_url(
    ws_base_url: &str,
    ticket: &str,
    metadata: &ClientMetadata,
) -> Result<String, String> {
    let mut url = Url::parse(ws_base_url).map_err(|error| error.to_string())?;
    if url.path().is_empty() || url.path() == "/" {
        url.set_path("/ws");
    }
    url.query_pairs_mut().append_pair("wsTicket", ticket);
    append_client_connection_params(&mut url, metadata);
    // `appendOrchestrationProtocol`: servers answer 426 to sockets without it.
    url.query_pairs_mut().append_pair(
        ORCHESTRATION_PROTOCOL_QUERY_PARAM,
        &ORCHESTRATION_PROTOCOL_VERSION.to_string(),
    );
    Ok(url.to_string())
}

/// The orchestration wire protocol this client speaks.
pub const ORCHESTRATION_PROTOCOL_VERSION: u32 = 2;
pub const ORCHESTRATION_PROTOCOL_QUERY_PARAM: &str = "orchestrationProtocol";
pub const ENVIRONMENT_DESCRIPTOR_PATH: &str = "/.well-known/t3/environment";

/// `ExecutionEnvironmentDescriptor`, served without authorization.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentDescriptor {
    #[serde(default)]
    pub environment_id: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub server_version: Option<String>,
    /// Servers shipped before negotiation omit it and speak version 1.
    #[serde(default)]
    pub orchestration_protocol_version: Option<u32>,
    #[serde(default)]
    pub capabilities: Value,
}

impl EnvironmentDescriptor {
    /// `orchestrationProtocolCompatibilityError`: why this client cannot talk to the server.
    pub fn incompatibility(&self) -> Option<String> {
        let server = self.orchestration_protocol_version.unwrap_or(1);
        let label = self.label.as_deref().unwrap_or("this environment");
        match server.cmp(&ORCHESTRATION_PROTOCOL_VERSION) {
            std::cmp::Ordering::Equal => None,
            std::cmp::Ordering::Greater => Some(format!(
                "This client is not supported by this server. Update your app or use a compatible release to connect to {label}."
            )),
            std::cmp::Ordering::Less => Some(format!(
                "This client requires a newer server. Update T3 Code on {label} to connect."
            )),
        }
    }
}

/// A redacted form of a URL for logs: the origin and path, never the query.
pub fn redact_url(url: &str) -> String {
    match Url::parse(url) {
        Ok(url) => format!("{}://{}{}", url.scheme(), display_host(&url), url.path()),
        Err(_) => "<invalid url>".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;
    use wiremock::matchers::{body_string, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn token_json() -> Value {
        json!({"access_token": "access-1", "issued_token_type": ACCESS_TOKEN_TYPE,
            "token_type": "Bearer", "expires_in": 3600, "scope": "orchestration:operate"})
    }

    fn metadata() -> ClientMetadata {
        ClientMetadata {
            label: Some("T3 Code Desktop".into()),
            device_type: Some("desktop".into()),
            os: Some("macos".into()),
            surface: Some("desktop".into()),
            app_version: Some("0.1.0".into()),
        }
    }

    #[test]
    fn endpoint_urls_replace_path_query_and_fragment() {
        assert_eq!(
            endpoint_url("https://box.example:8443/some/path?x=1#y", "/oauth/token")
                .unwrap()
                .as_str(),
            "https://box.example:8443/oauth/token"
        );
        assert!(endpoint_url("nope", "/x").is_err());
    }

    #[test]
    fn token_exchange_form_matches_upstream_fields() {
        let form = token_exchange_form("cred", Some("a b"), &metadata());
        let keys: Vec<_> = form.iter().map(|(key, _)| *key).collect();
        assert_eq!(
            keys,
            vec![
                "grant_type",
                "subject_token",
                "subject_token_type",
                "requested_token_type",
                "scope",
                "client_label",
                "client_device_type",
                "client_os"
            ]
        );
        let unknown_os = ClientMetadata {
            os: Some("other".into()),
            ..ClientMetadata::default()
        };
        assert_eq!(token_exchange_form("cred", None, &unknown_os).len(), 4);
        let unknown_os = ClientMetadata {
            os: Some("unknown".into()),
            ..ClientMetadata::default()
        };
        assert_eq!(token_exchange_form("cred", None, &unknown_os).len(), 4);
    }

    #[test]
    fn socket_urls_default_to_the_ws_path_and_carry_the_ticket() {
        assert_eq!(
            rpc_socket_url("ws://127.0.0.1:3773/", "t k", &ClientMetadata::default()).unwrap(),
            "ws://127.0.0.1:3773/ws?wsTicket=t+k&orchestrationProtocol=2"
        );
        assert_eq!(
            rpc_socket_url("wss://box.example/custom", "t", &metadata()).unwrap(),
            "wss://box.example/custom?wsTicket=t&clientSurface=desktop&clientAppVersion=0.1.0\
             &clientDeviceType=desktop&clientOs=macos&orchestrationProtocol=2"
        );
        let phone = ClientMetadata {
            device_type: Some("mobile".into()),
            ..Default::default()
        };
        assert!(
            rpc_socket_url("ws://a/", "t", &phone)
                .unwrap()
                .contains("clientDeviceType=phone&")
        );
        let bot = ClientMetadata {
            device_type: Some("bot".into()),
            ..Default::default()
        };
        assert!(
            rpc_socket_url("ws://a/", "t", &bot)
                .unwrap()
                .contains("clientDeviceType=unknown&")
        );
        assert!(rpc_socket_url("::", "t", &bot).is_err());
    }

    #[test]
    fn redacts_tickets_from_logged_urls() {
        assert_eq!(
            redact_url("ws://127.0.0.1:3773/ws?wsTicket=secret"),
            "ws://127.0.0.1:3773/ws"
        );
        assert_eq!(redact_url("garbage"), "<invalid url>");
    }

    #[test]
    fn classifies_transient_errors() {
        let status = |status| AuthError::UndeclaredStatus {
            url: "u".into(),
            status,
        };
        assert!(status(502).is_transient());
        assert!(status(503).is_transient());
        assert!(status(504).is_transient());
        assert!(!status(500).is_transient());
        assert!(!status(404).is_transient());
        assert!(
            AuthError::Fetch {
                url: "u".into(),
                message: "m".into()
            }
            .is_transient()
        );
        assert!(
            AuthError::Timeout {
                url: "u".into(),
                timeout: Duration::from_secs(1)
            }
            .is_transient()
        );
        assert!(!AuthError::InvalidJson { url: "u".into() }.is_transient());
        let invalid = classify_failure(
            "u".into(),
            401,
            br#"{"_tag":"EnvironmentAuthInvalidError","code":"auth_invalid","reason":"credential_invalid","traceId":"t"}"#,
        );
        assert!(!invalid.is_transient());
        assert!(invalid.is_unauthorized());
        assert_eq!(
            invalid.to_string(),
            "The environment rejected the credential (credential_invalid)."
        );
        assert_eq!(
            classify_failure("u".into(), 418, b"{\"_tag\":\"Teapot\"}"),
            status(418)
        );
        assert_eq!(classify_failure("u".into(), 500, b"<html>"), status(500));
    }

    #[test]
    fn protocol_compatibility_matches_upstream() {
        let descriptor = |version: Option<u32>| EnvironmentDescriptor {
            environment_id: None,
            label: Some("Box".into()),
            server_version: None,
            orchestration_protocol_version: version,
            capabilities: Value::Null,
        };
        assert_eq!(descriptor(Some(2)).incompatibility(), None);
        assert_eq!(
            descriptor(None).incompatibility().as_deref(),
            Some("This client requires a newer server. Update T3 Code on Box to connect.")
        );
        assert_eq!(
            descriptor(Some(3)).incompatibility().as_deref(),
            Some(
                "This client is not supported by this server. Update your app or use a compatible release to connect to Box."
            )
        );
    }

    #[tokio::test]
    async fn reads_the_environment_descriptor() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/.well-known/t3/environment"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "environmentId": "env-1", "label": "Box", "serverVersion": "0.1.7",
                "orchestrationProtocolVersion": 2, "capabilities": {"sideChats": true},
                "platform": {"os": "darwin"}})))
            .mount(&server)
            .await;
        let descriptor = AuthClient::default()
            .environment_descriptor(&server.uri())
            .await
            .unwrap();
        assert_eq!(descriptor.label.as_deref(), Some("Box"));
        assert_eq!(descriptor.orchestration_protocol_version, Some(2));
        assert_eq!(descriptor.incompatibility(), None);
    }

    #[tokio::test]
    async fn exchanges_a_bootstrap_credential() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth/token"))
            .and(header("content-type", "application/x-www-form-urlencoded"))
            .and(body_string(
                "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Atoken-exchange\
                 &subject_token=boot+strap\
                 &subject_token_type=urn%3At3%3Aparams%3Aoauth%3Atoken-type%3Aenvironment-bootstrap\
                 &requested_token_type=urn%3Aietf%3Aparams%3Aoauth%3Atoken-type%3Aaccess_token\
                 &client_label=T3+Code+Desktop&client_device_type=desktop&client_os=macos",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(token_json()))
            .expect(1)
            .mount(&server)
            .await;
        let token = AuthClient::default()
            .exchange_bootstrap_credential(&server.uri(), "boot strap", &metadata())
            .await
            .unwrap();
        assert_eq!(token.access_token, "access-1");
        assert_eq!(token.token_type, "Bearer");
        assert_eq!(token.expires_in, Some(3600.0));
    }

    #[tokio::test]
    async fn reports_declared_environment_errors() {
        let server = MockServer::start().await;
        Mock::given(path("/oauth/token"))
            .respond_with(ResponseTemplate::new(401).set_body_json(json!({
                "_tag": "EnvironmentAuthInvalidError", "code": "auth_invalid",
                "reason": "credential_expired", "traceId": "abc"})))
            .mount(&server)
            .await;
        let error = AuthClient::default()
            .exchange_bootstrap_credential(&server.uri(), "x", &ClientMetadata::default())
            .await
            .unwrap_err();
        assert_eq!(
            error,
            AuthError::Environment {
                status: 401,
                tag: "EnvironmentAuthInvalidError".into(),
                code: Some("auth_invalid".into()),
                reason: Some("credential_expired".into()),
            }
        );
    }

    #[tokio::test]
    async fn rejects_invalid_json_success_bodies() {
        let server = MockServer::start().await;
        Mock::given(path("/oauth/token"))
            .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
            .mount(&server)
            .await;
        let error = AuthClient::default()
            .exchange_bootstrap_credential(&server.uri(), "x", &ClientMetadata::default())
            .await
            .unwrap_err();
        assert!(matches!(error, AuthError::InvalidJson { .. }));
    }

    #[tokio::test]
    async fn times_out_slow_servers() {
        let server = MockServer::start().await;
        Mock::given(path("/oauth/token"))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(5)))
            .mount(&server)
            .await;
        let error = AuthClient::default()
            .with_timeout(Duration::from_millis(100))
            .exchange_bootstrap_credential(&server.uri(), "x", &ClientMetadata::default())
            .await
            .unwrap_err();
        assert!(matches!(error, AuthError::Timeout { .. }), "{error:?}");
    }

    #[tokio::test]
    async fn unreachable_servers_are_fetch_errors() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let error = AuthClient::default()
            .exchange_bootstrap_credential(
                &format!("http://127.0.0.1:{port}/"),
                "x",
                &ClientMetadata::default(),
            )
            .await
            .unwrap_err();
        assert!(matches!(error, AuthError::Fetch { .. }), "{error:?}");
        assert!(error.is_transient());
    }

    #[tokio::test]
    async fn retries_transient_failures_until_the_server_is_ready() {
        let server = MockServer::start().await;
        Mock::given(path("/oauth/token"))
            .respond_with(ResponseTemplate::new(503))
            .up_to_n_times(2)
            .mount(&server)
            .await;
        Mock::given(path("/oauth/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(token_json()))
            .mount(&server)
            .await;
        let token = AuthClient::default()
            .exchange_with_retry(
                &server.uri(),
                "x",
                &ClientMetadata::default(),
                Duration::from_secs(10),
            )
            .await
            .unwrap();
        assert_eq!(token.access_token, "access-1");
    }

    #[tokio::test]
    async fn does_not_retry_permanent_failures() {
        let server = MockServer::start().await;
        Mock::given(path("/oauth/token"))
            .respond_with(ResponseTemplate::new(400))
            .expect(1)
            .mount(&server)
            .await;
        let error = AuthClient::default()
            .exchange_with_retry(
                &server.uri(),
                "x",
                &ClientMetadata::default(),
                Duration::from_secs(10),
            )
            .await
            .unwrap_err();
        assert_eq!(
            error,
            AuthError::UndeclaredStatus {
                url: format!("{}/oauth/token", server.uri()),
                status: 400
            }
        );
    }

    #[tokio::test]
    async fn gives_up_retrying_when_the_budget_runs_out() {
        let server = MockServer::start().await;
        Mock::given(path("/oauth/token"))
            .respond_with(ResponseTemplate::new(502))
            .mount(&server)
            .await;
        let error = AuthClient::default()
            .exchange_with_retry(
                &server.uri(),
                "x",
                &ClientMetadata::default(),
                Duration::from_millis(700),
            )
            .await
            .unwrap_err();
        assert!(error.is_transient());
        let attempts = server.received_requests().await.unwrap().len();
        assert!((1..=3).contains(&attempts), "{attempts} attempts");
    }

    #[tokio::test]
    async fn issues_rpc_tickets_with_the_bearer_token() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/auth/websocket-ticket/rpc"))
            .and(header("authorization", "Bearer access-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(
                json!({"ticket": "ticket-1", "expiresAt": "2026-11-05T00:00:00.000Z"}),
            ))
            .expect(1)
            .mount(&server)
            .await;
        let url = AuthClient::default()
            .websocket_connection_url(
                &server.uri(),
                "ws://127.0.0.1:9/",
                "access-1",
                &ClientMetadata::default(),
            )
            .await
            .unwrap();
        assert_eq!(
            url,
            "ws://127.0.0.1:9/ws?wsTicket=ticket-1&orchestrationProtocol=2"
        );
    }

    #[tokio::test]
    async fn falls_back_to_the_bare_ticket_route_on_older_servers() {
        let server = MockServer::start().await;
        Mock::given(path("/api/auth/websocket-ticket/rpc"))
            .respond_with(ResponseTemplate::new(404))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(path("/api/auth/websocket-ticket"))
            .and(header("authorization", "Bearer access-1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ticket": "legacy"})))
            .expect(1)
            .mount(&server)
            .await;
        let ticket = AuthClient::default()
            .issue_rpc_ticket(&server.uri(), "access-1")
            .await
            .unwrap();
        assert_eq!(ticket.ticket, "legacy");
    }

    #[tokio::test]
    async fn does_not_fall_back_on_other_failures() {
        let server = MockServer::start().await;
        Mock::given(path("/api/auth/websocket-ticket/rpc"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        Mock::given(path("/api/auth/websocket-ticket"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ticket": "legacy"})))
            .expect(0)
            .mount(&server)
            .await;
        let error = AuthClient::default()
            .issue_rpc_ticket(&server.uri(), "a")
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            AuthError::UndeclaredStatus { status: 500, .. }
        ));
    }
}
