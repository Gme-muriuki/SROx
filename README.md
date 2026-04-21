# SROx

A reverse proxy built for **Speed, Reliability, and Observability** — from first principles, in Rust.

> Single binary. Async. TLS-terminating. Built to handle real traffic and explain itself while doing it.

---

## What it does

- Terminates TLS (rustls, TLS 1.2+ only)
- Routes requests by host + path prefix, longest-prefix wins
- Caches responses in-memory (LRU, byte-bounded, stale-while-revalidate)
- Protects upstreams with circuit breakers and idempotent retries
- Emits Prometheus metrics, structured JSON logs, and OpenTelemetry traces — from request one

## What it does not do (v1)

Multi-instance deployment, shared cache, dynamic config reload, WAF, auth, rate limiting, HTTP/2 upstream, TLS passthrough, Windows.

---

## Quick start

```bash
# Build
cargo build --release

# Run with a config file
./target/release/srox --config config.toml

# Health check
curl --insecure https://localhost:8443/healthz

# Metrics
curl http://localhost:9090/metrics
```

**Minimum requirements:** Linux, Rust 1.77+. Check your file descriptor limit before running under load (`ulimit -n`).

---

## Configuration

```toml
[server]
bind     = "0.0.0.0:8443"
tls_cert = "certs/cert.pem"
tls_key  = "certs/key.pem"

[cache]
max_bytes        = 536870912  # 512 MB
default_ttl_secs = 60

[[routes]]
host     = "api.example.com"
prefix   = "/v1"
upstream = "backend"

[upstreams.backend]
addrs                    = ["127.0.0.1:8080"]
pool_size                = 10
keepalive_secs           = 60
timeout_secs             = 2
health_check_interval_secs = 5

[circuit_breaker]
error_rate_threshold = 0.5  # 50% failures
latency_threshold_secs = 2.0
min_request_count    = 10
window_secs          = 10
half_open_probes     = 3
```

SROx validates the full config before applying it. An invalid config at startup exits cleanly with an error. It never panics on bad input.

---

## Observability

Every request gets a `trace_id` at the listener. It appears in every log line, every span, and the `X-Trace-Id` response header.

**Logs** — structured JSON:

```json
{
  "ts": "2026-04-04T12:00:00.000Z",
  "level": "info",
  "trace_id": "4bf92f3577b34da6a3ce929d0e0e4736",
  "method": "GET",
  "path": "/api/users",
  "upstream": "backend",
  "status": 200,
  "duration_ms": 4.2,
  "cache_status": "HIT"
}
```

**Metrics** — exposed at `/metrics`:

| Metric | Type | Description |
| --- | --- | --- |
| `srox_request_duration_seconds` | histogram | p50/p95/p99 by method, status, upstream, cache_status |
| `srox_active_connections` | gauge | Open client connections |
| `srox_upstream_healthy` | gauge | 0 or 1 per upstream |
| `srox_circuit_state` | gauge | 0=closed, 1=open, 2=half-open per upstream |
| `srox_pool_connections_active` | gauge | In-use connections per upstream |
| `srox_stale_revalidations_in_flight` | gauge | Background revalidation tasks running |

**Traces** — OTLP to Jaeger. Set `OTEL_EXPORTER_OTLP_ENDPOINT` to point at your collector.

---

## Cache behavior

Cache key: `METHOD + HOST + PATH + QUERY_STRING + sorted(Vary fields)`

- `Vary` headers are respected. Different `Accept-Encoding` values are cached separately.
- Responses to requests with `Authorization` are **not cached** unless upstream returns `Cache-Control: public`.
- `Cache-Control: no-store` is never cached. `Cache-Control: no-cache` is stored but always revalidated.
- `HEAD` and `GET` responses are cached independently. A HEAD hit never satisfies a GET.
- `X-Cache-Status: HIT | MISS | STALE` is set on every response.

---

## Benchmarks

Targets — to be measured and filled in after Phase 3:

| Scenario | p99 |
| --- | --- |
| Cache HIT | ~2ms |
| Cache MISS | ~45ms |
| Cache hit ratio (static assets) | 80% |

If the numbers differ from these targets, the design doc explains why.

---

## Project structure

```
srox/
├── src/
│   ├── main.rs           # entry point
│   ├── config.rs         # TOML config + validation
│   ├── tls.rs            # rustls setup
│   ├── http_codec.rs     # HTTP/1.1 parsing, chunked, keep-alive
│   ├── router.rs         # host + prefix trie
│   ├── cache/            # LRU, stale-while-revalidate
│   ├── pool.rs           # connection pool + health checks
│   ├── circuit.rs        # circuit breaker state machine
│   ├── retry.rs          # idempotent retry + backoff
│   ├── headers.rs        # request/response header sanitization
│   └── telemetry/        # Prometheus + OpenTelemetry
├── bench/                # benchmark harness and results
├── docs/
│   ├── DESIGN.md         # full design document
│   └── postmortem-*.md   # post-mortems, added as they happen
├── tests/                # integration tests
└── config.toml           # example config
```

---

## Development

```bash
# Run tests
cargo test

# Run integration tests
cargo test --test '*'

# Run benchmarks
cargo bench

# Lint
cargo clippy -- -D warnings

# Audit dependencies
cargo audit
```

---

## Design document

The full design — goals, non-goals, architecture, key decisions with trade-offs, failure modes, and phase breakdown — is in [`docs/DESIGN.md`](docs/DESIGN.md).

If something in the code differs from the design doc, the design doc gets updated with an explanation. That delta is the post-mortem material.

---

## License

MIT
