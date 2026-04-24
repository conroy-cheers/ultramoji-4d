use std::{
    net::SocketAddr,
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use axum::{
    Router,
    body::Body,
    extract::{Query, State},
    http::{
        HeaderMap, HeaderValue, Method, StatusCode, Uri,
        header::{
            ACCEPT, ACCEPT_ENCODING, CACHE_CONTROL, CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE,
            ETAG, LAST_MODIFIED, LOCATION, REFERRER_POLICY, VARY, X_CONTENT_TYPE_OPTIONS,
        },
    },
    response::{IntoResponse, Response},
    routing::get,
};
use clap::Parser;
use futures_util::StreamExt;
use percent_encoding::percent_decode_str;
use serde::Deserialize;
use tokio::{fs, net::TcpListener};
use tracing::{error, info, warn};

const STATIC_CACHE_CONTROL: &str = "public, max-age=31536000, immutable";
const DOCUMENT_CACHE_CONTROL: &str = "no-cache";
const RELAY_CACHE_CONTROL: &str = "public, max-age=86400";
const DEFAULT_RELAY_TIMEOUT_SECS: u64 = 20;
const DEFAULT_RELAY_MAX_BYTES: usize = 8 * 1024 * 1024;
const SERVER_NAME: &str = "UltramojiServer/0.1";

#[derive(Parser, Debug)]
#[command(about = "Serve emoji-web with Slack emoji asset relay")]
struct Args {
    #[arg(long, default_value = "127.0.0.1", help = "Bind address")]
    bind: String,

    #[arg(
        long,
        env = "EMOJI_WEB_PORT",
        default_value_t = 8765,
        help = "Port to listen on"
    )]
    port: u16,
}

#[derive(Clone)]
struct AppState {
    static_dir: Arc<PathBuf>,
    client: reqwest::Client,
    relay_max_bytes: usize,
}

#[derive(Deserialize)]
struct RelayQuery {
    url: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_target(false)
        .without_time()
        .init();

    let args = Args::parse();
    let static_dir = std::env::var_os("EMOJI_WEB_STATIC_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(default_static_dir)
        .canonicalize()?;
    let relay_timeout = Duration::from_secs(env_u64(
        "EMOJI_WEB_RELAY_TIMEOUT_SECS",
        DEFAULT_RELAY_TIMEOUT_SECS,
    ));
    let relay_max_bytes = env_usize("EMOJI_WEB_RELAY_MAX_BYTES", DEFAULT_RELAY_MAX_BYTES);
    let client = reqwest::Client::builder()
        .timeout(relay_timeout)
        .user_agent("ultramoji-4d-emoji-web/1.0")
        .build()?;

    let state = AppState {
        static_dir: Arc::new(static_dir),
        client,
        relay_max_bytes,
    };

    let app = Router::new()
        .route("/healthz", get(healthz))
        .route("/emoji-asset", get(relay_emoji_asset))
        .fallback(static_asset)
        .with_state(state.clone());

    let addr: SocketAddr = format!("{}:{}", args.bind, args.port).parse()?;
    let listener = TcpListener::bind(addr).await?;
    info!(
        "serving emoji-web on http://{} from {}",
        listener.local_addr()?,
        state.static_dir.display()
    );
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

async fn shutdown_signal() {
    if let Err(err) = tokio::signal::ctrl_c().await {
        warn!("failed to install Ctrl-C handler: {err}");
    }
}

async fn healthz() -> Response {
    let mut response = Response::new(Body::from("ok\n"));
    let headers = response.headers_mut();
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    add_security_headers(headers);
    response
}

async fn relay_emoji_asset(
    State(state): State<AppState>,
    Query(query): Query<RelayQuery>,
) -> Response {
    let Ok(url) = reqwest::Url::parse(&query.url) else {
        return error_response(StatusCode::BAD_REQUEST, "invalid Slack emoji asset URL");
    };
    if !is_allowed_asset_url(&url) {
        return error_response(StatusCode::BAD_REQUEST, "invalid Slack emoji asset URL");
    }

    let upstream = match state
        .client
        .get(url.clone())
        .header(
            ACCEPT,
            "image/avif,image/webp,image/apng,image/svg+xml,image/*,*/*;q=0.8",
        )
        .send()
        .await
    {
        Ok(upstream) => upstream,
        Err(err) => {
            warn!(%url, %err, "emoji relay upstream request failed");
            return error_response(StatusCode::BAD_GATEWAY, "emoji relay failed");
        }
    };

    if let Some(content_length) = upstream.content_length() {
        if content_length > state.relay_max_bytes as u64 {
            return error_response(StatusCode::PAYLOAD_TOO_LARGE, "emoji asset too large");
        }
    }

    let status = upstream.status();
    let upstream_headers = upstream.headers().clone();
    let mut body = Vec::new();
    let mut stream = upstream.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(err) => {
                warn!(%url, %err, "emoji relay upstream body failed");
                return error_response(StatusCode::BAD_GATEWAY, "emoji relay failed");
            }
        };
        if body.len().saturating_add(chunk.len()) > state.relay_max_bytes {
            return error_response(StatusCode::PAYLOAD_TOO_LARGE, "emoji asset too large");
        }
        body.extend_from_slice(&chunk);
    }

    let mut builder = Response::builder().status(status);
    let headers = builder.headers_mut().expect("response builder headers");
    for name in [CONTENT_TYPE, ETAG, LAST_MODIFIED, CACHE_CONTROL] {
        if let Some(value) = upstream_headers.get(&name) {
            headers.insert(name, value.clone());
        }
    }
    if !headers.contains_key(CACHE_CONTROL) {
        headers.insert(CACHE_CONTROL, HeaderValue::from_static(RELAY_CACHE_CONTROL));
    }
    headers.insert(
        CONTENT_LENGTH,
        HeaderValue::from_str(&body.len().to_string()).expect("valid content length"),
    );
    add_security_headers(headers);
    builder
        .body(Body::from(body))
        .expect("valid relay response")
}

async fn static_asset(
    State(state): State<AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
) -> Response {
    if method != Method::GET && method != Method::HEAD {
        return error_response(StatusCode::METHOD_NOT_ALLOWED, "method not allowed");
    }

    let Some((asset_path, redirect_to)) = resolve_static_path(&state.static_dir, uri.path()).await
    else {
        return error_response(StatusCode::NOT_FOUND, "file not found");
    };
    if let Some(location) = redirect_to {
        let mut response = Response::builder()
            .status(StatusCode::MOVED_PERMANENTLY)
            .header(LOCATION, location)
            .header(CACHE_CONTROL, DOCUMENT_CACHE_CONTROL)
            .body(Body::empty())
            .expect("valid redirect response");
        add_security_headers(response.headers_mut());
        return response;
    }

    let served_path = choose_precompressed_path(&asset_path, &headers).await;
    let body = match fs::read(&served_path.path).await {
        Ok(body) => body,
        Err(err) => {
            error!(path = %served_path.path.display(), %err, "failed to read static asset");
            return error_response(StatusCode::NOT_FOUND, "file not found");
        }
    };
    let metadata = match fs::metadata(&served_path.path).await {
        Ok(metadata) => metadata,
        Err(err) => {
            error!(path = %served_path.path.display(), %err, "failed to stat static asset");
            return error_response(StatusCode::NOT_FOUND, "file not found");
        }
    };

    let mime = mime_guess::from_path(&asset_path).first_or_octet_stream();
    let mut builder = Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, mime.as_ref())
        .header(CONTENT_LENGTH, body.len().to_string())
        .header(CACHE_CONTROL, cache_control_for(&asset_path));
    if let Ok(modified) = metadata.modified() {
        builder = builder.header(LAST_MODIFIED, httpdate::fmt_http_date(modified));
    }
    if let Some(encoding) = served_path.content_encoding {
        builder = builder
            .header(CONTENT_ENCODING, encoding)
            .header(VARY, "Accept-Encoding");
    }
    let response_body = if method == Method::HEAD {
        Body::empty()
    } else {
        Body::from(body)
    };
    let mut response = builder.body(response_body).expect("valid static response");
    add_security_headers(response.headers_mut());
    response
}

struct ServedPath {
    path: PathBuf,
    content_encoding: Option<&'static str>,
}

async fn choose_precompressed_path(path: &Path, headers: &HeaderMap) -> ServedPath {
    let accept_encoding = headers
        .get(ACCEPT_ENCODING)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    for (encoding, suffix) in [("br", "br"), ("gzip", "gz")] {
        if !accepts_encoding(accept_encoding, encoding) {
            continue;
        }
        let candidate = PathBuf::from(format!("{}.{}", path.display(), suffix));
        if fs::metadata(&candidate)
            .await
            .map(|metadata| metadata.is_file())
            .unwrap_or(false)
        {
            return ServedPath {
                path: candidate,
                content_encoding: Some(encoding),
            };
        }
    }
    ServedPath {
        path: path.to_owned(),
        content_encoding: None,
    }
}

async fn resolve_static_path(
    static_dir: &Path,
    request_path: &str,
) -> Option<(PathBuf, Option<String>)> {
    let decoded = percent_decode_str(request_path).decode_utf8().ok()?;
    let mut relative = PathBuf::new();
    for component in Path::new(decoded.trim_start_matches('/')).components() {
        match component {
            Component::Normal(part) => relative.push(part),
            Component::CurDir => {}
            _ => return None,
        }
    }

    let mut path = static_dir.join(relative);
    if fs::metadata(&path)
        .await
        .map(|metadata| metadata.is_dir())
        .unwrap_or(false)
    {
        if !request_path.ends_with('/') {
            return Some((path, Some(format!("{request_path}/"))));
        }
        path = path.join("index.html");
    }
    if fs::metadata(&path)
        .await
        .map(|metadata| metadata.is_file())
        .unwrap_or(false)
    {
        Some((path, None))
    } else if is_spa_route(&decoded) {
        let index_path = static_dir.join("index.html");
        fs::metadata(&index_path)
            .await
            .map(|metadata| metadata.is_file())
            .unwrap_or(false)
            .then_some((index_path, None))
    } else {
        None
    }
}

fn is_spa_route(decoded_path: &str) -> bool {
    let mut components = Path::new(decoded_path.trim_start_matches('/')).components();
    matches!(
        components.next(),
        Some(Component::Normal(component)) if component == "emoji"
    )
}

fn accepts_encoding(header: &str, encoding: &str) -> bool {
    header
        .split(',')
        .filter_map(|part| part.trim().split(';').next())
        .any(|part| part.eq_ignore_ascii_case(encoding))
}

fn cache_control_for(path: &Path) -> &'static str {
    if path
        .file_name()
        .is_some_and(|name| name == "index.html" || name == "asset-manifest.json")
    {
        return DOCUMENT_CACHE_CONTROL;
    }
    let path = path.to_string_lossy();
    if path.contains("/pkg/")
        || path.ends_with(".js")
        || path.ends_with(".wasm")
        || path.ends_with(".css")
        || path.ends_with(".ttf")
    {
        STATIC_CACHE_CONTROL
    } else {
        DOCUMENT_CACHE_CONTROL
    }
}

fn is_allowed_asset_url(url: &reqwest::Url) -> bool {
    if url.scheme() != "https" || url.username() != "" || url.password().is_some() {
        return false;
    }
    let Some(host) = url.host_str().map(str::to_ascii_lowercase) else {
        return false;
    };
    ["slack-edge.com", "slack-files.com"]
        .iter()
        .any(|suffix| host == *suffix || host.ends_with(&format!(".{suffix}")))
}

fn add_security_headers(headers: &mut HeaderMap) {
    headers.insert("server", HeaderValue::from_static(SERVER_NAME));
    headers.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers.insert(REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    headers.insert(
        "cross-origin-resource-policy",
        HeaderValue::from_static("same-origin"),
    );
}

fn error_response(status: StatusCode, message: &'static str) -> Response {
    let mut response = (status, message).into_response();
    add_security_headers(response.headers_mut());
    response
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn env_usize(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn default_static_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|path| {
            path.parent()
                .map(|parent| parent.join("../share/ultramoji/static"))
        })
        .unwrap_or_else(|| PathBuf::from("static"))
}
