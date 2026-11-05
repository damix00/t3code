//! [`Connector`]s for the sync engine: the local backend this app supervises, and a remote
//! environment paired with a code.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use t3_client::{AuthClient, ClientMetadata, Connection, Connector, Keepalive};
use tokio::sync::Mutex;

use crate::supervisor::Backend;

/// The upstream desktop retries its bearer exchange for this long while the backend starts.
const BEARER_RETRY_BUDGET: Duration = Duration::from_secs(15);
/// A cold server with a large database can take a while to accept the socket.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

/// Authorizes once and caches the bearer token; a refused ticket or upgrade clears it so the
/// next attempt exchanges a fresh credential.
#[derive(Debug, Default)]
pub(crate) struct BearerCache {
    token: Mutex<Option<String>>,
}

impl BearerCache {
    pub(crate) async fn get_or_exchange<F, Fut>(&self, exchange: F) -> Result<String, String>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<String, String>>,
    {
        let mut token = self.token.lock().await;
        if let Some(token) = token.as_ref() {
            return Ok(token.clone());
        }
        let fresh = exchange().await?;
        *token = Some(fresh.clone());
        Ok(fresh)
    }

    async fn clear(&self) {
        *self.token.lock().await = None;
    }

    pub(crate) async fn is_set(&self) -> bool {
        self.token.lock().await.is_some()
    }
}

/// Fails early, with upstream's wording, when the server speaks another protocol version.
async fn check_compatible(auth: &AuthClient, http_base_url: &str) -> Result<(), String> {
    let descriptor = auth
        .environment_descriptor(http_base_url)
        .await
        .map_err(|error| error.to_string())?;
    match descriptor.incompatibility() {
        Some(reason) => Err(reason),
        None => Ok(()),
    }
}

pub(crate) async fn open_socket(
    auth: &AuthClient,
    bearer: &BearerCache,
    http_base_url: &str,
    ws_base_url: &str,
    bearer_token: String,
    metadata: &ClientMetadata,
) -> Result<Connection, String> {
    let url = match auth
        .websocket_connection_url(http_base_url, ws_base_url, &bearer_token, metadata)
        .await
    {
        Ok(url) => url,
        Err(error) => {
            if error.is_unauthorized() {
                bearer.clear().await;
            }
            return Err(error.to_string());
        }
    };
    match Connection::connect(&url, CONNECT_TIMEOUT, Keepalive::default()).await {
        Ok(connection) => Ok(connection),
        Err(error) => {
            if error.is_unauthorized() {
                bearer.clear().await;
            }
            Err(error.to_string())
        }
    }
}

/// Connects to the backend this app launched, once it is ready.
#[derive(Debug, Clone)]
pub struct LocalConnector {
    backend: Backend,
    auth: AuthClient,
    metadata: ClientMetadata,
    bearer: Arc<BearerCache>,
}

impl LocalConnector {
    pub fn new(backend: Backend) -> Self {
        Self {
            backend,
            auth: AuthClient::default(),
            metadata: ClientMetadata::desktop(),
            bearer: Arc::default(),
        }
    }
}

impl Connector for LocalConnector {
    fn connect(&self) -> Pin<Box<dyn Future<Output = Result<Connection, String>> + Send + '_>> {
        Box::pin(async move {
            self.backend
                .wait_ready()
                .await
                .map_err(|error| error.to_string())?;
            let http_base_url = self.backend.http_base_url();
            check_compatible(&self.auth, http_base_url).await?;
            let bearer = self
                .bearer
                .get_or_exchange(|| async {
                    self.auth
                        .exchange_with_retry(
                            http_base_url,
                            &self.backend.credential(),
                            &self.metadata,
                            BEARER_RETRY_BUDGET,
                        )
                        .await
                        .map(|token| token.access_token)
                        .map_err(|error| error.to_string())
                })
                .await?;
            open_socket(
                &self.auth,
                &self.bearer,
                http_base_url,
                &self.backend.ws_base_url(),
                bearer,
                &self.metadata,
            )
            .await
        })
    }
}

/// Connects to a paired remote environment. The pairing credential works once, so the bearer
/// token it buys is kept for the life of this connector.
#[derive(Debug, Clone)]
pub struct RemoteConnector {
    http_base_url: String,
    ws_base_url: String,
    auth: AuthClient,
    metadata: ClientMetadata,
    bearer: Arc<BearerCache>,
    credential: Arc<Mutex<Option<String>>>,
}

impl RemoteConnector {
    /// From a resolved pairing target.
    pub fn new(target: t3_client::PairingTarget) -> Self {
        Self {
            http_base_url: target.http_base_url,
            ws_base_url: target.ws_base_url,
            auth: AuthClient::default(),
            metadata: ClientMetadata::desktop(),
            bearer: Arc::default(),
            credential: Arc::new(Mutex::new(Some(target.credential))),
        }
    }

    /// From a bearer token saved earlier.
    pub fn with_bearer(http_base_url: String, ws_base_url: String, bearer: String) -> Self {
        Self {
            http_base_url,
            ws_base_url,
            auth: AuthClient::default(),
            metadata: ClientMetadata::desktop(),
            bearer: Arc::new(BearerCache {
                token: Mutex::new(Some(bearer)),
            }),
            credential: Arc::new(Mutex::new(None)),
        }
    }

    pub fn http_base_url(&self) -> &str {
        &self.http_base_url
    }

    /// The bearer token, once paired, so it can be saved.
    pub async fn bearer_token(&self) -> Option<String> {
        self.bearer.token.lock().await.clone()
    }
}

impl Connector for RemoteConnector {
    fn connect(&self) -> Pin<Box<dyn Future<Output = Result<Connection, String>> + Send + '_>> {
        Box::pin(async move {
            check_compatible(&self.auth, &self.http_base_url).await?;
            let bearer = self
                .bearer
                .get_or_exchange(|| async {
                    let credential = self.credential.lock().await.take().ok_or_else(|| {
                        "this environment no longer accepts the saved session; pair again"
                            .to_owned()
                    })?;
                    match self
                        .auth
                        .exchange_bootstrap_credential(
                            &self.http_base_url,
                            &credential,
                            &self.metadata,
                        )
                        .await
                    {
                        Ok(token) => Ok(token.access_token),
                        Err(error) => {
                            // A transient failure may be retried with the same credential.
                            if error.is_transient() {
                                *self.credential.lock().await = Some(credential);
                            }
                            Err(error.to_string())
                        }
                    }
                })
                .await?;
            open_socket(
                &self.auth,
                &self.bearer,
                &self.http_base_url,
                &self.ws_base_url,
                bearer,
                &self.metadata,
            )
            .await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn bearer_cache_exchanges_once_until_cleared() {
        let cache = BearerCache::default();
        let calls = AtomicUsize::new(0);
        let exchange = || async {
            let call = calls.fetch_add(1, Ordering::SeqCst);
            Ok::<_, String>(format!("token-{call}"))
        };
        assert_eq!(cache.get_or_exchange(exchange).await, Ok("token-0".into()));
        assert_eq!(cache.get_or_exchange(exchange).await, Ok("token-0".into()));
        cache.clear().await;
        assert_eq!(cache.get_or_exchange(exchange).await, Ok("token-1".into()));
    }

    #[tokio::test]
    async fn bearer_cache_does_not_store_failures() {
        let cache = BearerCache::default();
        assert_eq!(
            cache
                .get_or_exchange(|| async { Err::<String, _>("nope".into()) })
                .await,
            Err("nope".into())
        );
        assert_eq!(
            cache
                .get_or_exchange(|| async { Ok::<_, String>("ok".into()) })
                .await,
            Ok("ok".into())
        );
    }
}
