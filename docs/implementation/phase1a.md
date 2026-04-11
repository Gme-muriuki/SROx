# Phase 1a — TLS + Request Forwarding

> Your only job this phase: a client sends an HTTPS request, SROx forwards it to an upstream, the response comes back. That's it. Nothing else ships until that works and is tested.

---

## How to think about this phase

You are not building a proxy yet. You are building the **foundation that the proxy will stand on**. Every decision you make here — how you handle errors, how you structure config, how you log — will echo through every phase that follows.

A production engineer starting a new service does not write the feature first and clean up later. They ask: _what does this need to be correct, observable, and safe to run?_ Answer that question before you write the function.

**Production mindset for this phase means:**

- No `unwrap()` in any code path that can be reached by a client. Panics take down the whole proxy, not just one connection.
- Every error is either handled or propagated with context. "Connection reset by peer" is not a crash — it is a log line and a continue.
- The config is the single source of truth. The bind address lives in one place. Not in main, not hardcoded in a test, not in two places that can drift.
- A test is not "I ran it and it seemed to work." A test is an assertion that will catch a regression six weeks from now when you have forgotten this code exists.
- If something is temporary, it is either a `// TODO(phase-2):` comment with a reason, or it does not exist. No silent shortcuts.

---

## What you are building

```
Client (HTTPS)
     │
     ▼ TCP accept
[ TcpListener ]
     │
     ▼ TLS handshake (rustls)
[ TlsStream ]
     │
     ▼ HTTP/1.1 header parse (httparse)
     │   - read request line + headers
     │   - validate: no ambiguous framing
     │   - reject: Content-Length + Transfer-Encoding together → 400
     │   - reject: HTTP/1.0 → 505
     │
     ▼ Forward to upstream (plain TCP for now)
[ Upstream ]
     │
     ▼ Read response, write back to client
[ Client ]
```

No caching. No circuit breaker. No pool. One upstream, hardcoded in config. That comes later.

---

## The order you build it

### Step 1 — Config struct

Before a listener, before a socket, write the config.

```rust
// src/config.rs

pub struct Config {
    pub bind_addr: SocketAddr,
    pub tls: TlsConfig,
    pub upstream: UpstreamConfig,
}

pub struct TlsConfig {
    pub cert_path: PathBuf,
    pub key_path:  PathBuf,
}

pub struct UpstreamConfig {
    pub addr: SocketAddr,
}
```

Write a `Config::from_file(path)` that:

1. Reads the TOML file
2. Parses it into the struct
3. Validates it — does the cert file exist? Is the bind address valid?
4. Returns a clean error if anything is wrong

**The rule:** if `Config::from_file` returns `Ok`, the config is usable. If it returns `Err`, the process should log the error and exit. No partial configs, no defaults that silently mask a misconfiguration.

---

### Step 2 — TCP listener

```rust
// src/listener.rs
```

Write a loop that:

- Binds to `config.bind_addr`
- Calls `listener.accept()` in a loop
- On `Ok((stream, addr))` — spawns a Tokio task to handle the connection
- On `Err(e)` — logs the error and **continues the loop**

The `Err` case is the important one. `accept()` fails on transient OS errors. That is not a reason to bring down the proxy. Log it, keep going.

```rust
loop {
    match listener.accept().await {
        Ok((stream, addr)) => {
            tokio::spawn(handle_connection(stream, addr, config.clone()));
        }
        Err(e) => {
            // log the error — do not panic, do not break
            tracing::error!(error = %e, "accept failed");
        }
    }
}
```

---

### Step 3 — TLS handshake

Take the raw `TcpStream` and wrap it in a TLS acceptor.

```rust
// src/tls.rs

pub fn build_acceptor(config: &TlsConfig) -> Result<TlsAcceptor, ConfigError> {
    // load cert chain
    // load private key
    // build ServerConfig with TLS 1.2 minimum
    // return TlsAcceptor
}
```

In `handle_connection`:

```rust
let tls_stream = match acceptor.accept(tcp_stream).await {
    Ok(s) => s,
    Err(e) => {
        tracing::warn!(error = %e, "TLS handshake failed");
        return; // client goes away, proxy keeps running
    }
};
```

A failed TLS handshake is not an error in your proxy. It is a bad client. Log it at `warn`, not `error`. Keep the distinction — `error` means something is wrong with SROx. `warn` means something was wrong with a client.

---

### Step 4 — HTTP/1.1 parsing

Read bytes from the TLS stream. Parse headers with `httparse`.

You are looking for:

- Request method, path, HTTP version
- All headers

Reject immediately with the correct status code if:

| Condition                                                     | Response                              |
| ------------------------------------------------------------- | ------------------------------------- |
| Both `Content-Length` and `Transfer-Encoding` present         | `400 Bad Request`                     |
| HTTP version is 1.0 or lower                                  | `505 HTTP Version Not Supported`      |
| Headers exceed a size limit (set a limit — 8KB is reasonable) | `431 Request Header Fields Too Large` |
| Parse fails entirely                                          | `400 Bad Request`                     |

These are not edge cases. These are the smuggling boundary. Enforce them from day one.

---

### Step 5 — Forward to upstream

Open a plain TCP connection to `config.upstream.addr`. Write the parsed request. Read the response. Write it back to the client.

No pool yet. A new TCP connection per request is fine for Phase 1a. The pool comes in Phase 2. What matters now is that the forwarding path works and the bytes are correct.

---

### Step 6 — Write the tests

Do not move on until these pass:

**Test 1 — TCP connection is accepted**
Bind the listener. Open a raw TCP connection. Assert the connection is accepted without error.

**Test 2 — TLS handshake completes**
Use `rcgen` to generate a self-signed cert in the test. Connect with a TLS client that trusts that cert. Assert the handshake succeeds.

**Test 3 — Invalid framing is rejected**
Send a request with both `Content-Length` and `Transfer-Encoding`. Assert you get back `400 Bad Request`.

**Test 4 — HTTP/1.0 is rejected**
Send an HTTP/1.0 request. Assert you get back `505`.

**Test 5 — Valid request is forwarded**
Spin up a trivial TCP echo server as the upstream. Send a valid GET request through SROx. Assert the response comes back correctly.

**Test 6 — Accept loop survives a bad client**
Connect to the listener and immediately drop the connection before the TLS handshake. Assert the listener is still accepting new connections afterward.

---

## What the deliverable looks like

```bash
# This works:
curl --insecure https://localhost:8443/api/test
# Response from upstream

# This is rejected:
curl --insecure https://localhost:8443/ \
  -H "Content-Length: 5" \
  -H "Transfer-Encoding: chunked"
# HTTP 400

# All tests pass:
cargo test
# test result: ok. 6 passed; 0 failed
```

---

## What you are not building yet

Do not be tempted to add these. They have a phase.

- Connection pooling — Phase 2
- Health checks — Phase 2
- Caching — Phase 3
- Circuit breaker — Phase 4
- Retry logic — Phase 4
- OpenTelemetry traces — Phase 1b
- Prometheus metrics — Phase 1b

If you find yourself reaching for any of these, write a `// TODO(phase-N):` comment and move on. The discipline of not over-building is as important as the discipline of building correctly.

---

## Files you will create

```
src/
├── main.rs         — parse config, start listener, block on runtime
├── config.rs       — Config struct, from_file(), validation
├── listener.rs     — accept loop, task spawning
├── tls.rs          — TlsAcceptor builder, cert/key loading
├── http_codec.rs   — httparse wrapper, request parsing, rejection logic
└── error.rs        — your error types (use thiserror)

tests/
└── phase1a.rs      — the six tests above

config.toml         — example config for local development
certs/              — gitignored, generated by rcgen in tests
```

---

## Questions to answer before you write a function

1. What does success look like for this function?
2. What are the ways it can fail?
3. What should happen to the connection — and to the proxy — when it fails?
4. How will I know it works?

If you cannot answer all four, you are not ready to write the function yet.

---

## Revision history

| Date       | Note                                                         |
| ---------- | ------------------------------------------------------------ |
| April 2026 | Phase 1a plan written pre-implementation                     |
| —          | Update after implementation: what matched, what changed, why |
