//! HTTP client integration used by GPUI resources such as remote Markdown images.

use futures::{AsyncReadExt, FutureExt, future::BoxFuture};
use gpui::http_client::http::HeaderValue;
use gpui::http_client::{AsyncBody, HttpClient, Request, Response, Url, read_proxy_from_env};
use reqwest::Client;
use std::any::type_name;
use std::sync::Arc;

/// User-Agent sent by Aster's application-level HTTP client.
const ASTER_USER_AGENT: &str = concat!("Aster/", env!("CARGO_PKG_VERSION"));

/// Adapts the locked Reqwest client to GPUI's asynchronous HTTP client contract.
pub struct ReqwestHttpClient {
    /// Shared Reqwest connection pool used for all application HTTP requests.
    client: Client,
    /// Dedicated Tokio runtime required by Reqwest's asynchronous network stack.
    runtime: Arc<tokio::runtime::Runtime>,
    /// Proxy discovered from the process environment for GPUI diagnostics and consumers.
    proxy: Option<Url>,
    /// Stable User-Agent value exposed through GPUI's HTTP client contract.
    user_agent: HeaderValue,
}

impl ReqwestHttpClient {
    /// Creates a configured HTTP client with HTTPS, redirect, and environment proxy support.
    pub fn new() -> anyhow::Result<Arc<Self>> {
        let client = Client::builder().user_agent(ASTER_USER_AGENT).build()?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;

        Ok(Arc::new(Self {
            client,
            runtime: Arc::new(runtime),
            proxy: read_proxy_from_env(),
            user_agent: HeaderValue::from_static(ASTER_USER_AGENT),
        }))
    }
}

impl HttpClient for ReqwestHttpClient {
    /// Returns the concrete client type for GPUI diagnostics.
    fn type_name(&self) -> &'static str {
        type_name::<Self>()
    }

    /// Returns the User-Agent sent by this client.
    fn user_agent(&self) -> Option<&HeaderValue> {
        Some(&self.user_agent)
    }

    /// Sends a GPUI request through Reqwest and converts its response body back to `AsyncBody`.
    fn send(
        &self,
        request: Request<AsyncBody>,
    ) -> BoxFuture<'static, anyhow::Result<Response<AsyncBody>>> {
        let client = self.client.clone();
        let runtime = self.runtime.clone();
        async move {
            runtime
                .spawn(send_request(client, request))
                .await
                .map_err(|error| anyhow::anyhow!("HTTP request task failed: {error}"))?
        }
        .boxed()
    }

    /// Returns the proxy discovered from the process environment, if any.
    fn proxy(&self) -> Option<&Url> {
        self.proxy.as_ref()
    }
}

/// Reads a GPUI request body into bytes before handing it to Reqwest.
async fn read_request_body(mut body: AsyncBody) -> anyhow::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    body.read_to_end(&mut bytes).await?;
    Ok(bytes)
}

/// Sends one request and converts the Reqwest response to GPUI's response type.
async fn send_request(
    client: Client,
    request: Request<AsyncBody>,
) -> anyhow::Result<Response<AsyncBody>> {
    let (parts, body) = request.into_parts();
    let body = read_request_body(body).await?;
    let response = client
        .request(parts.method, parts.uri.to_string())
        .headers(parts.headers)
        .body(body)
        .send()
        .await?;

    let status = response.status().as_u16();
    let headers = response.headers().clone();
    let body = response.bytes().await?.to_vec();

    let mut response_builder = Response::builder().status(status);
    if let Some(target_headers) = response_builder.headers_mut() {
        *target_headers = headers;
    }

    Ok(response_builder.body(AsyncBody::from(body))?)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies that the client exposes the same stable User-Agent it sends.
    #[test]
    fn exposes_aster_user_agent() {
        let client = ReqwestHttpClient::new().expect("client should initialize");

        assert_eq!(client.user_agent().unwrap(), ASTER_USER_AGENT);
    }

    /// Verifies that GPUI's in-memory request body can be forwarded to Reqwest.
    #[test]
    fn reads_async_body_bytes() {
        let body = futures::executor::block_on(read_request_body(AsyncBody::from(
            "image request body".to_string(),
        )))
        .expect("in-memory body should be readable");

        assert_eq!(body, b"image request body");
    }

    /// Verifies that a GPUI request can complete through the configured HTTP client.
    #[test]
    fn sends_request_and_returns_response_body() {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::thread;

        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("test server should bind");
        let address = listener
            .local_addr()
            .expect("test server should expose an address");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("test server should accept");
            let mut request = [0; 1024];
            let _ = stream.read(&mut request);
            let body = b"image response";
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\n\r\n",
                body.len()
            )
            .expect("test response headers should be written");
            stream
                .write_all(body)
                .expect("test response body should be written");
        });

        let client = ReqwestHttpClient::new().expect("client should initialize");
        let request = Request::builder()
            .uri(format!("http://{address}/image.png"))
            .body(AsyncBody::default())
            .expect("test request should build");
        let response = futures::executor::block_on(client.send(request))
            .expect("configured client should send the request");
        let mut body = Vec::new();
        futures::executor::block_on(response.into_body().read_to_end(&mut body))
            .expect("response body should be readable");
        server.join().expect("test server should finish");

        assert_eq!(body, b"image response");
    }

    /// Verifies that the client reports the proxy selected from the same environment.
    #[test]
    fn reports_environment_proxy() {
        let expected_proxy = read_proxy_from_env();
        let client = ReqwestHttpClient::new().expect("client should initialize");

        assert_eq!(client.proxy(), expected_proxy.as_ref());
    }
}
