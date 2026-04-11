## SROx

A production-aware reverse proxy built for Speed, Reliability, and Observability - from first principles, in Rust.

| Author        | Date           | Status |
| ------------- | -------------- | ------ |
| James Muriuki | 4th April 2026 | Draft  |

---
| S                                                                       | R                                                                             | O                                                                                      |
| ----------------------------------------------------------------------- | ----------------------------------------------------------------------------- | -------------------------------------------------------------------------------------- |
| **SPEED**                                                               | **RELIABILITY**                                                               | **OBSERVABILITY**                                                                      |
| Sub-millisecond cache hits. Connection reuse. Zero-copy where possible. | Circuit breakers. Retries. Graceful upstream failure. Stale-while-revalidate. | Every request traced. Prometheus metrics from day one. Structured logs with trace IDs. |


### Problem Statement

Modern web infrastructure needs a reverse proxy that does more than forward requests. It must terminate TLS, absorb upstream failure, cache aggressively, and surface enough telemetry to debug anything -- all without becoming the bottleneck it was built to prevent.

Existing proxies fail in predictable ways. Nginx mishandles HTTP desync at protocol boundaries.
Envoy is operationally heavy for teams that don't have a platform org behind it.
Home-built proxies skip observability until something breaks in production and there is nothing to look at.

SROx is built to be the version that learned from those mistakes. It is a single-binary, async Rust reverse proxy designed to handle real traffic - with design docs, failure post-mortems, and benchmarks to prove it.

### Goals and Non-Goals

##### GOALS - WHAT SROx WILL DO

- ✅ Terminate TLS using **rustls** (pure Rust, memory-safe)
- ✅ Route request via **host + path prefix matching**, longest prefix wins
- ✅ Maintain **per-upstream connection pools** with health checks.
- ✅ Cache response in-memory with **LRU + TLL + stale-while-revalidate**
- ✅ Protect upstreams with circuit breakers and idempotent retries
- ✅ Emit **Prometheus metrics** and structured logs from request one
- ✅ Propagate **OpenTelemetry trace IDs** across every request hop
- ✅ Load config from TOML; **validate before applying**, never crash on bad input
- ✅ Be benchmarked: report p50, p95 latency and throughput for each phase

##### NON-GOALS - WHAT SROx WILL NOT DO (atleast v1)

- ❌ Multi-instance deployment or shared cache (v1 is single-process)
- ❌ HTTP/2 or HTTP/3 upstream connections
- ❌ Dynamic config reload without restart
- ❌ Web Application Firewall (WAF) rules
- ❌ Authentication or JWT validation
- ❌ Rate limiting or DDoS mitigation
- ❌ TLS passthrough (SROx always terminates)
- ❌ Windows support


### Architecture Overview

SROx is a single Tokio async process. Every inbound connection is a task. Upstream connections are pooled per-route. 
The hot path - listen -> decode -> route -> cache chec -> upstream -> respond - is kept clean.
Observability wraps the hot path; it does not pollute it.

    -----------------------------------------------------------------------
                           SROx — Request Lifecycle
    ------------------------------------------------------------------------

                     Client (HTTPS)
                          │
                          ▼  TLS handshake (rustls)
                     [ Listener ]  ── one Tokio task per connection ──▶  file descriptor limit: watch this
                          │
                          ▼  HTTP/1.1 decode (httparse)
                     [ Router ]   ── host + longest-prefix path match
                          │
                          ├─── 404 if no route matches
                          │
                          ▼  cache key = METHOD + HOST + PATH
                     [ Cache ]    ── LRU, bounded by bytes, not item count
                          │
                          ├─── HIT  (fresh)        ──▶  respond immediately   X-Cache-Status: HIT
                          ├─── HIT  (stale)        ──▶  respond + background revalidate (one task, not one per request)
                          │
                          ▼  MISS
                     [ Upstream Manager ]
                          │
                          ├─── connection pool (max 10, keep-alive 60s)
                          ├─── health check (active, every 5s)
                          ├─── circuit breaker  CLOSED → OPEN → HALF-OPEN
                          │         open after: 5 failures in 10s
                          │         probe:      3–5 requests in half-open (not 1)
                          │         trip on:    errors AND slow responses (latency threshold)
                          │
                          ├─── retry (idempotent methods only: GET, HEAD, OPTIONS)
                          │         max 3 attempts, exponential backoff + jitter
                          │
                          ▼
                     [ Upstream A ]  [ Upstream B ]  [ Upstream C ]
                      health: up      health: down    health: up
                      circuit: ok     circuit: OPEN   circuit: ok
                          │
                          ▼
                     [ Response pipeline ]
                          │
                          ├─── write to cache (if cacheable)
                          ├─── emit span to OTel exporter
                          ├─── increment Prometheus counters (low-cardinality labels only)
                          ├─── write structured log line (with trace_id)
                          │
                          ▼
                     Client
    --------------------------------------------------------------------
          Observability runs alongside every step -- not bolted on after.
          Prometheus scrapes /metrics. Jaeger receives OTLP traces.
    --------------------------------------------------------------------

---
### Core components

---


### Key Design Decisions

1. TLS LIBRARY:
   - rustls over openssl
 - Options: rustls, openssl (via rust-openssl), native-tls
 - Choice: **rustls**,
 - Why: Pure Rust, memory safe, no C FFI.
     - TLS 1.2+ only (no legacy cipher baggage). Active development, good benchmark numbers at high thread count
 - Trade-off: Doesn't support all legacy cipher suits. Acceptable -- SROx targets modern clients. Documented in non-goals

2. CACHING STRATEGY:
   - In-memory LRU over Redis
 - Options: In-memory LRU, Redis, Memcached.
 - Choice: **In-memory LRU**
 - Why: Benchmarks target: p99~2ms(local) vs ~45ms(Redis round-trip).
    - For single-instance proxy no shared cache is needed.
    - Restart clears cache -- acceptable for v1.
 - Trade-off: Cache is lost on restart. Not shared across processes. Both documented as v2 concerns.

3. CACHE EVICTION:
   - LRU bounded by bytes
 - Options: Item count bound, byte bound, LFU, SLRU
 - Choice: **LRU + byte bound**
 - Why: Item count allows one large response to silently consume disproportionate memory. Byte bound is predictable. LRU is simple and debuggable.
     - SLRU would be a v2 optimisation.
 - Trade-off: LRU is vulnerable to sequential scan eviction. Documented limitation. Nonscan workloads expected in v1.

4. STALE CONTENT
    - stale-while-revalidate
  - Options: Strict TTL expiry, serve-stale, stale-while-revalidate
  - Choice: **stale-while-revalidate**
  - Why: Eliminates cache-miss latency spikes for users. Background revalidation is triggered once per stale key -- not once per concurrent request. Stampede-safe by design.
  - Trade-off: Users may briefly see stale content. Acceptable for static assets.

5. CIRCUIT BREAKER
    - Latency-aware, sliding window
  - Options: Error-count only, error-rate only, latency+error combined
  - Choice: **Error rate + latency, sliding window**
  - Why: A slow upstream that never errors still causes cascading failures via thread starvation. Latency threashold catches this. Sliding winwo with minimum call count avoids tripping on 1-of-2 failures (statistically meaningless).
  - Trade-off: More complex to implement and tune. Half-open uses 3-5 probe requests, not 1, for reliable recovery signal

6. RETRIES
    - Idempotent methods only
  - Options: Retry all failures, retry idempotent only, no retries
  - Choice: **GET**, **HEAD**, **Options** only
  - Why: Retrying POST/PUT risks duplicate mutations. GET retries are safe by definition. Jitter on backoff prevents thundering-herd on upstream recovery
  - Trade-off: Non-idempotent failures return immediately to the client. Client must implement its own retry logic for POST/PUT.

7. METRICS CARDINALITY
    - Low-cardinality labels only
  - Options: Full URL as label, per-user metrics, route + method + status
  - Choice: **method, status_code, upstream, cache_status**
  - Why: High-cardinality labels (full URL, user ID, request ID) cause cardinality explosion in Prometheus -- millions of time series, memory exhaustion, can even be used as a DoS vector. Trace IDs belong in traces, not metrics.
  - Trade-off: Cannot drill down to individual request metrics. Use traces for that -- that is their job.

8. CONFIG VALIDATION
    - Validate before applying, never crash
  - Options: Parse-and-apply, validate-then-apply, schema-only validation
  - Choice: **Validate before applying**
  - Why: A missing comma in a config value took down a LinkedIn proxy fleet fleet-wide because the proxy crashed on startup after fetching the bad config. SROx valiates fully before applying, and falls back to last-known-good on error.
  - Trade-off: Startup is slightly slower. Worthwhile though.


### What could go wrong.

These are known failure modes, documented before a line of code is written.
Each one has a mitigation built into the design. If something from this list causes a real outage during development, it becomes a post-moterm

##### Failure mode:
1. File descriptor exhaustion
##### Severity:
- High
##### How SROx addresses it
- Document the FD limit requirement. Expose active connection count as a Prometheus metric. Alert when approaching limit.
---
##### Failure mode:
2. Cache stampede on expiry
##### Severity:
- High
##### How SROx addresses it
- stale-while-revalidate with a single background task per stale key. Concurrent requests to the same stale key all receive the stale response -- only one triggers revaliation
---
##### Failure mode:
3. HTTP desync / request smuggling
##### Severity:
- High
##### How SROx handles it
- Be strict and explicit about Content-length vs Transfer-Encoding handling. Never allow ambiguity to pass through. Document the parsing policy.
---
##### Failure mode:
4. Circuit breaker flapping
##### Severity:
- Medium
##### How SROx handles it
- Minimum call count before evaluation. Sliding time window. 3-5 probe request in half-open state, not 1. Expose circuit state as a Prometheus metric.
---
##### Failure mode:
5. TLS session resumption mutex contention
##### Severity:
- Medium
##### How SROx handles it
- Use rustls >= 0.23.17 which replaced the mutex with an RwLock on ticket rotation. Document minimum rustls version in Cargo.toml
---
##### Failure mode:
6. URL normalization inconsistency
##### Severity:
- Medium
##### How SROx handles it
- Document explicitly what SROx normalizes, encodes, and passes through verbatim. Never silently transform URLs. Write a test for each behaviour.
---
##### Failure mode:
7. Prometheus cardinality explosion
##### Severity:
- Medium
##### How SROx handles it
- Only low-cardinality labels (method, status code, upstream name, cache_status). Never use full URLs, user IDs, or request IDs as label values. Write a test that asserts label count.
---
##### Failure mode:
8. Slow upstream (not failed) causing cascading threadpool starvation
##### Severity: 
- High
##### How SROx handles it
- Circuit breaker trips on latency not just errors. Per-request timeout enforced at the connection pool level. Timout duration configurable per route.
---
##### Failure mode:
9. Bad config crashes the proxy on startup
##### Severity:
- High
##### How SROx handles it
- Config fully validated before applicatio. On invalid config, log the error and fall back to last-known-good. Never crash on a config parse failure.
---
##### Failure mode:
10. New upstream receives cold-start traffic flood
##### Severity:
- Low
##### How SROx handles it
- Health check must pass before the upstream receives production traffic. Document warm-up behaviour. Future: slow-start weight ramp.


### Phase Breakdown

Each phase has one clear deliverable. A phase is not done until the deliverable works and the benchmark numbers are written down.
Design doc gets updates after each phase with what reality taught us.

##### **Phase 1**:
- **Duration**
  -  Week 1-3
-  **Description**
   -  Minimal HTTP reverse proxy
      -  TCP listener + rustls + HTTP/1.1 forwarding to a single upstream. Structured logging with trace_id from request one. Config validation in place.
- **Deliverable**
    - `curl https://localhost:8443/api/test` -> response from upstream, trace_id in logs
---
##### **Phase 2**:
- **Duration**
  - Week 4-6
- **Description**
  - Connection pools + health checks + Prometheus
    - Per-upstream connection pool (max 10, keep-alive 60s). Active health checks every 5s. Prometheus `/metrics` endpoint with low-cardinality labels only
- **Deliverable**
  - Kill upstream -> proy_upstream_health flips to 0 in prometheus within 10s
---
##### **Phase 3**:
- **Duration**
  - Week 7-9
- **Desciption**
  - In-memory LRU cache
    - Byte-bounded LRU. Cache  key = method + host + path. TTL from Cache-Control header. stale-while-revalidate with single background task. X-Cache-Status header on every response.
- **Deliverable**
  - 80% hit ratio. p99 cache hit ~2ms. p99 cache miss ~45ms. Number written down.
---
##### **Phase 4**:
- **Duration**
  - Week 10-12
- **Description**
  - Circuit breakers + retries + OpenTelemetry
    - Circuit breaker state machine (Closed -> Open -> Half-Open). Trips on errors AND latency. Retry with backoff + jitter for idempotent methods. OpenTelemetry trace export to Jaeger. Design doc updated with what changed and why.
- **Deliverable**
  - Jaeger show full trace. `cargo test --test` circuit_breaker passes. Design doc reflects final decisions.


### Future Work

Deliberately out of scope for v1. Not forgotten - just sequenced corretly.

##### v2
1. HTTP/2 upstream
  - Multiplexed streams. Required for modern backend services. Adds complexity to connection pooling.

2. Dynamic config reload
  - Apply config changes without restart. Requires careful state management for in-flight requests.
  
3. Shared cache layer
   - Multi-instance deployments needs a shared cache (Redis or custom). Out of scope until single-instance is proven.

##### v3
1. Segmented LRU (SLRU)
   - Protects the hot working set from large sequential scans. LRU is documented as having this weakness in v1.

2. Upstream slow-start
   - Gradually ramp traffic to new upstreams to prevent cold-start floods. Documented as a known risk in v1.
  
3. Least-connections LB
   - More accurate than round-robin under variable request duration. Requires per-connection tracking overhead.


### Revision History
