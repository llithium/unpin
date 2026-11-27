//! Local review server.
//!
//! Serves the comparison report from `127.0.0.1`, keeps review decisions in a
//! file through [`ReviewStore`], and checks with Pinterest whether pins
//! flagged for removal are really gone. It never changes the Pinterest
//! account: removal still happens on Pinterest itself.
//!
//! Every route sits under a random path token printed in the terminal, and
//! requests must name this server's own host, so other websites and DNS
//! rebinding cannot read or change review data.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;

use axum::Router;
use axum::extract::{DefaultBodyLimit, Path, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post, put};
use serde::{Deserialize, Serialize};
use serde_json::json;
use thiserror::Error;
use tokio::net::TcpListener;

use crate::pinterest::{PinPresence, PinterestClient};
use crate::report::Report;
use crate::review_store::{DecisionChanges, ReviewStore, ReviewStoreError};

/// Tried first so the address, and the browser's per-site settings for it,
/// stay the same between runs. Any free port is used when it is taken.
pub const DEFAULT_PORT: u16 = 7171;
const MAX_BODY_BYTES: usize = 64 * 1024 * 1024;
/// Pinterest checks are spaced out by the client's own limiter too; this
/// keeps "Check all" on a large cleanup list from bursting.
const PIN_CHECK_CONCURRENCY: usize = 4;

#[derive(Debug, Error)]
pub enum ServerError {
    #[error("could not start the review server on 127.0.0.1")]
    Bind(#[source] std::io::Error),
    #[error("the review server stopped")]
    Serve(#[source] std::io::Error),
}

struct AppState {
    report: Report,
    client: Option<PinterestClient>,
    store: ReviewStore,
    base: String,
    port: u16,
    checks: tokio::sync::Semaphore,
}

pub struct ReviewServer {
    listener: TcpListener,
    router: Router,
    url: String,
}

impl ReviewServer {
    /// Binds the server. `port` of `None` tries [`DEFAULT_PORT`] and then any
    /// free port; `Some(0)` always uses a free port.
    pub async fn bind(
        report: Report,
        client: Option<PinterestClient>,
        store: ReviewStore,
        port: Option<u16>,
    ) -> Result<Self, ServerError> {
        let listener = match port {
            Some(port) => TcpListener::bind((Ipv4Addr::LOCALHOST, port)).await,
            None => match TcpListener::bind((Ipv4Addr::LOCALHOST, DEFAULT_PORT)).await {
                Ok(listener) => Ok(listener),
                Err(_) => TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await,
            },
        }
        .map_err(ServerError::Bind)?;
        let address: SocketAddr = listener.local_addr().map_err(ServerError::Bind)?;
        let token = hex::encode(rand::random::<[u8; 16]>());
        let base = format!("/{token}");
        let url = format!("http://127.0.0.1:{}{base}/", address.port());
        let state = Arc::new(AppState {
            report,
            client,
            store,
            base: base.clone(),
            port: address.port(),
            checks: tokio::sync::Semaphore::new(PIN_CHECK_CONCURRENCY),
        });
        let router = Router::new()
            .route(&format!("{base}/"), get(page))
            .route(&format!("{base}/api/decisions"), put(update_decisions))
            .route(&format!("{base}/api/pins/{{pin_id}}"), get(check_pin))
            .route(
                &format!("{base}/api/pins/{{pin_id}}/removed"),
                post(mark_removed),
            )
            .route("/", get(landing))
            .fallback(not_found)
            .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
            .layer(middleware::from_fn_with_state(state.clone(), guard))
            .with_state(state);
        Ok(Self {
            listener,
            router,
            url,
        })
    }

    /// The address to open, including the path token.
    pub fn url(&self) -> &str {
        &self.url
    }

    pub async fn run(self) -> Result<(), ServerError> {
        axum::serve(self.listener, self.router)
            .await
            .map_err(ServerError::Serve)
    }
}

/// Rejects requests for another host name (DNS rebinding) and cross-site
/// writes, and keeps every response out of caches and referrers.
async fn guard(State(state): State<Arc<AppState>>, request: Request, next: Next) -> Response {
    let own_hosts = [
        format!("127.0.0.1:{}", state.port),
        format!("localhost:{}", state.port),
    ];
    // Computed in a block so no borrow of the request, whose body is not
    // `Sync`, is held across the await below.
    let allowed = {
        let header_value = |name: header::HeaderName| -> Option<String> {
            request
                .headers()
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned)
        };
        let host_ok = header_value(header::HOST).is_some_and(|host| own_hosts.contains(&host));
        let writes = !matches!(request.method().as_str(), "GET" | "HEAD");
        let origin_ok = header_value(header::ORIGIN).is_none_or(|origin| {
            own_hosts
                .iter()
                .any(|host| origin == format!("http://{host}"))
        });
        let json_ok = header_value(header::CONTENT_TYPE)
            .is_some_and(|value| value.split(';').next() == Some("application/json"));
        host_ok && (!writes || (origin_ok && json_ok))
    };
    let mut response = if !allowed {
        StatusCode::FORBIDDEN.into_response()
    } else {
        next.run(request).await
    };
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    response
}

async fn landing() -> Html<&'static str> {
    Html(
        "<!doctype html><meta charset=utf-8><title>unpin</title><p>Open the review link printed in your terminal.</p>",
    )
}

async fn not_found() -> StatusCode {
    StatusCode::NOT_FOUND
}

async fn page(State(state): State<Arc<AppState>>) -> Response {
    let saved = match state.store.load().await {
        Ok(saved) => saved,
        Err(error) => return store_error(error),
    };
    let config = json!({
        "api": format!("{}/api", state.base),
        "state": saved,
        "checks": state.client.is_some(),
        "signed_in": state.client.as_ref().is_some_and(PinterestClient::is_authenticated),
        "store_path": state.store.path().display().to_string(),
    });
    Html(crate::visual::render_review_page(&state.report, &config)).into_response()
}

async fn update_decisions(State(state): State<Arc<AppState>>, body: axum::body::Bytes) -> Response {
    let Ok(changes) = serde_json::from_slice::<DecisionChanges>(&body) else {
        return (StatusCode::BAD_REQUEST, "invalid JSON").into_response();
    };
    match state.store.apply(changes).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => store_error(error),
    }
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PinCheckState {
    Present,
    Removed,
    Unknown,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PinCheck {
    pub pin_id: String,
    pub state: PinCheckState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub board: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

async fn check_pin(State(state): State<Arc<AppState>>, Path(pin_id): Path<String>) -> Response {
    if !valid_pin_id(&pin_id) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let Some(client) = &state.client else {
        return axum::Json(PinCheck {
            pin_id,
            state: PinCheckState::Unknown,
            board: None,
            message: Some("This report was opened without a Pinterest connection.".into()),
        })
        .into_response();
    };
    let result = {
        let _permit = state.checks.acquire().await.expect("never closed");
        client.pin_presence(&pin_id).await
    };
    let check = match result {
        Ok(PinPresence::Missing) => PinCheck {
            state: PinCheckState::Removed,
            board: None,
            message: None,
            pin_id,
        },
        Ok(PinPresence::Present { board }) => PinCheck {
            state: PinCheckState::Present,
            board,
            message: None,
            pin_id,
        },
        Err(error) => PinCheck {
            state: PinCheckState::Unknown,
            board: None,
            message: Some(error.to_string()),
            pin_id,
        },
    };
    let removed = match check.state {
        PinCheckState::Removed => Some(true),
        PinCheckState::Present => Some(false),
        PinCheckState::Unknown => None,
    };
    if let Some(removed) = removed
        && let Err(error) = state.store.set_removed(&check.pin_id, removed).await
    {
        return store_error(error);
    }
    axum::Json(check).into_response()
}

#[derive(Deserialize)]
struct RemovedMark {
    removed: bool,
}

async fn mark_removed(
    State(state): State<Arc<AppState>>,
    Path(pin_id): Path<String>,
    body: axum::body::Bytes,
) -> Response {
    let Ok(mark) = serde_json::from_slice::<RemovedMark>(&body) else {
        return (StatusCode::BAD_REQUEST, "invalid JSON").into_response();
    };
    if !valid_pin_id(&pin_id) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    match state.store.set_removed(&pin_id, mark.removed).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => store_error(error),
    }
}

fn store_error(error: ReviewStoreError) -> Response {
    let status = match error {
        ReviewStoreError::InvalidDecision(_) => StatusCode::BAD_REQUEST,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    (status, headers, error.to_string()).into_response()
}

/// Pinterest pin IDs are decimal numbers.
fn valid_pin_id(pin_id: &str) -> bool {
    (1..=32).contains(&pin_id.len()) && pin_id.bytes().all(|byte| byte.is_ascii_digit())
}
