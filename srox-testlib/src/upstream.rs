//! A programmable HTTP/1.1 upstream server for integration tests.
//!
//! ## Design goals
//!
//! **Zero external state.** Each `MockUpstream::start()` call produces
//! an independent server on an OS-assigned port. Tests never share an
//! upstream.
//!
//! **Request recording.** Every HTTP request received is stored in a
//! thread-safe queue that test code can drain and inspect.
//!
//! **Programmable responses.** Push [`MockResponse`] values onto the
//! queue before making requests. The server pops one per request. When
//! the queue is empty it returns a default 200 OK.
//!
//! **Keep-alive.** The server loops over multiple requests per TCP
//! connection so that pool-reuse tests see the same source port on
//! repeated requests.
//!
//! **Health endpoint.** `GET /healthz` returns 200 when healthy and
//! 503 when unhealthy. Toggle with `[MockUpstream::set_healthy]`.
//!
//! **Connection accounting.** [`MockUpstream::unique_connection_count`]
//! counts distinct TCP connections ever accepted -- useful for asserting
//! that the pool really did reuse a connection rather than opening a new
//! one.

use std::{
    collections::VecDeque,
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::Mutex,
};

/// A single HTTP request as the upstream received it.
#[derive(Debug, Clone)]
pub struct ReceivedRequest {
    /// First-line method token, e.g `"GET"`
    pub method: String,
    /// Request target (path + query), e.g `"/api/users?page=2"`
    pub path: String,
    /// All headers as `(name, value)` pairs. Names are lowercased.
    pub headers: Vec<(String, String)>,
    /// Source port of the TCP connection -- used to detect connection reuse
    /// (two requests with the same source port arrived over the same connection).
    pub source_port: u16,
}

impl ReceivedRequest {
    /// Return the value of the first header with the given name.
    pub fn header(&self, name: &str) -> Option<&str> {
        let lower = name.to_lowercase();
        self.headers
            .iter()
            .find(|(key, _)| key == &lower)
            .map(|(_, val)| val.as_str())
    }
}

/// A canned response the mock upstream will send
///
/// If not specified, each field falls back to the [`Default`]:
/// status 200, `Content-Length: 2`, body `"OK"`, no delay.
#[derive(Debug, Clone)]
pub struct MockResponse {
    pub status: u16,
    /// Extra headers to include (name, value). 'Content-Length' is set
    /// automatically from `body.len()` if not present here.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,

    /// Artificial delay before sending the response. Useful for timeout
    /// tests.
    pub delay_ms: u64,
}

impl Default for MockResponse {
    fn default() -> Self {
        Self {
            status: 200,
            headers: Vec::new(),
            body: b"OK".to_vec(),
            delay_ms: 0,
        }
    }
}

impl MockResponse {
    pub fn ok() -> Self {
        Self::default()
    }

    pub fn status(status: u16) -> Self {
        Self {
            status,
            ..Default::default()
        }
    }

    pub fn with_body(body: impl Into<Vec<u8>>) -> Self {
        Self {
            body: body.into(),
            ..Default::default()
        }
    }

    pub fn with_delay(delay_ms: u64) -> Self {
        Self {
            delay_ms,
            ..Default::default()
        }
    }
}

// ---------------------- Internal Shared State -------------

struct MockState {
    request: Mutex<Vec<ReceivedRequest>>,
    response_queue: Mutex<VecDeque<MockResponse>>,
    connection_counter: AtomicUsize,
    healthy: AtomicBool,
}

// ---------------------- Public Handle -----------------------

pub struct MockUpstream {
    /// The address the upstream is listening on.
    pub addr: SocketAddr,
    state: Arc<MockState>,
}

impl MockUpstream {
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("mock upstream: failed to bind");

        let addr = listener.local_addr().unwrap();

        let state = Arc::new(MockState {
            request: Mutex::new(Vec::new()),
            response_queue: Mutex::new(VecDeque::new()),
            connection_counter: AtomicUsize::new(0),
            healthy: AtomicBool::new(true),
        });

        let state_for_task = Arc::clone(&state);

        tokio::spawn(async move {
            accept_loop(listener, state_for_task).await;
        });

        MockUpstream { addr, state }
    }

    //-------------- Response configuration------------------

    /// Queue a response. The next request (that it ain't a health check)
    /// will receive it. Request beyond the queue receive `MockResponse::default()`
    pub async fn push_response(&self, resp: MockResponse) {
        self.state.response_queue.lock().await.push_back(resp);
    }

    /// Queue `n` identical responses
    pub async fn push_responses(&self, resp: MockResponse, n: usize) {
        let mut mresp = self.state.response_queue.lock().await;

        for _ in 0..n {
            mresp.push_back(resp.clone());
        }
    }

    // ----------------------- Health -----------------------

    /// Toggle the health state. Affects `GET /healthz` responses.
    pub fn set_healthy(&self, val: bool) {
        self.state.healthy.store(val, Ordering::Relaxed);
    }

    pub fn is_healthy(&self) -> bool {
        self.state.healthy.load(Ordering::Relaxed)
    }

    // --------------------- Request Inspection ----------------

    /// Drain and return all requests received so far.
    pub async fn drain_requests(&self) -> Vec<ReceivedRequest> {
        let mut guard = self.state.request.lock().await;

        std::mem::take(&mut *guard)
    }

    /// Return a snapshot of all requests received so far (non-destructive).
    pub async fn received_requests(&self) -> Vec<ReceivedRequest> {
        self.state.request.lock().await.clone()
    }
    /// Number of requests received (including health checks).
    pub async fn request_count(&self) -> usize {
        self.state.request.lock().await.len()
    }

    // ----------------- Connection accounting ---------------

    /// Total number of distinct TCP connections ever accepted.
    ///
    /// Used to verify connection-pool reuse: if two requests were served
    /// and this returns 1, both came over the same TCP connection.
    pub fn unique_connection_count(&self) -> usize {
        self.state.connection_counter.load(Ordering::SeqCst)
    }

    /// Wait until at least `n` requests have been received, polling every
    /// 20 ms.
    ///
    /// Times out after 5 seconds.
    pub async fn wait_for_request(&self, n: usize) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);

        loop {
            if self.request_count().await >= n {
                return;
            }

            if tokio::time::Instant::now() > deadline {
                panic!(
                    "mock upstream: timed out waiting fro {} requests (got {})",
                    n,
                    self.request_count().await
                );
            }

            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

// ----------------- Accept + connection loop ------------------

async fn accept_loop(listener: TcpListener, state: Arc<MockState>) {
    while let Ok((stream, addr)) = listener.accept().await {
        state.connection_counter.fetch_add(1, Ordering::SeqCst);

        let st = Arc::clone(&state);
        tokio::spawn(handle_connection(stream, addr, st));
    }
}

/// Handle one TCP connection. Loops over requests to support keep-alive.
async fn handle_connection(mut stream: TcpStream, addr: SocketAddr, state: Arc<MockState>) {
    let source_port = addr.port();

    loop {
        // Read until we have a complete header section.
        let mut buf = Vec::with_capacity(4096);
        let header_end;

        loop {
            let mut tmp = [0u8; 4096];
            let n = match stream.read(&mut tmp).await {
                Ok(0) => return,
                Ok(n) => n,
                Err(_) => return,
            };

            buf.extend_from_slice(&tmp[..n]);

            if let Some(pos) = find_header_end(&buf) {
                header_end = pos;
                break;
            }

            if buf.len() > 64 * 1024 {
                // Request too large - send 431 and close
                let _ = stream.write_all(b"HTTP/1.1 431 Request Header Fields Too Large\r\nContent-Length:0\r\nConnection: close\r\n\r\n").await;

                return;
            }
        }

        // Parse the request.
        let header_bytes = &buf[..header_end];
        let (method, path, headers) = parse_header_section(header_bytes);

        let is_health = path == "/healthz";

        let wants_close = headers
            .iter()
            .any(|(k, v)| k == "connection" && v.eq_ignore_ascii_case("close"));

        // Record the request (including health checks -- tests may assert on them).
        state.request.lock().await.push(ReceivedRequest {
            method: method.clone(),
            path: path.clone(),
            headers: headers.clone(),
            source_port,
        });

        // Build the response.

        let response_bytes = if is_health {
            if state.healthy.load(Ordering::Relaxed) {
                b"HTTP/1.1 200 Ok\r\nContent-Length: 2\r\nConnection:keep-alive\r\n\r\nOk".to_vec()
            } else {
                b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: keep-alive\r\n\r\n".to_vec()
            }
        } else {
            let resp = {
                let mut mresp = state.response_queue.lock().await;

                mresp.pop_front().unwrap_or_default()
            };

            if resp.delay_ms > 0 {
                tokio::time::sleep(Duration::from_millis(resp.delay_ms)).await;
            };

            build_response(&resp, wants_close)
        };

        if stream.write_all(&response_bytes).await.is_err() {
            return;
        }

        if wants_close {
            return;
        }
    }
}

// ------------------ Helpers ----------------------

/// Find the byte offset just past `\r\n\r\n` (the end of HTTP headers).
fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4)
        .position(|wnd| wnd == b"\r\n\r\n")
        .map(|pos| pos + 4)
}

/// Extract (method, path, headers) from the raw header bytes.
fn parse_header_section(hbytes: &[u8]) -> (String, String, Vec<(String, String)>) {
    let text = String::from_utf8_lossy(hbytes);
    let mut lines = text.lines();

    let first = lines.next().unwrap_or("");
    let mut parts = first.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("").to_string();

    let headers = lines
        .filter_map(|line| {
            let (name, value) = line.split_once(":")?;

            Some((name.trim().to_lowercase(), value.trim().to_string()))
        })
        .collect();

    (method, path, headers)
}

/// Serialize a [`MockResponse`] into raw HTTP bytes.
fn build_response(resp: &MockResponse, wants_close: bool) -> Vec<u8> {
    let reason = match resp.status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        400 => "Bad Request",
        404 => "Not Found",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "Unknown",
    };

    let conn_header = if wants_close {
        "Connection: close"
    } else {
        "Connection: keep-alive"
    };

    let mut out = format!(
        "HTTP/1.1 {} {}\r\nContent-Length: {}\r\n{}\r\n",
        resp.status,
        reason,
        resp.body.len(),
        conn_header,
    );

    for (name, value) in &resp.headers {
        out.push_str(&format!("{}: {}\r\n", name, value));
    }

    out.push_str("\r\n");

    let mut bytes = out.into_bytes();
    bytes.extend_from_slice(&resp.body);

    bytes
}
