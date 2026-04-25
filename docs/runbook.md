# SROx — Local Development Runbook

> Run these in order. Each step assumes the previous one is running.

---

## Prerequisites

- Docker running
- `cargo` installed
- You are in the `srox/srox` directory (the crate root, where `Cargo.toml` lives)
- `config.toml` and `certs/` are present

---

## Step 1 — Start upstream (Python HTTP server)

Open **Terminal 1**:

```bash
python3 -m http.server 8080
```

Leave it running. This is the upstream SROx proxies to.

---

## Step 2 — Start Jaeger (traces)

Open **Terminal 2**:

```bash
docker run --rm -it \
  -p 16686:16686 \
  -p 4317:4317 \
  -e COLLECTOR_OTLP_ENABLED=true \
  jaegertracing/all-in-one:latest
```

| What               | Where                  |
| ------------------ | ---------------------- |
| Jaeger UI          | http://localhost:16686 |
| OTLP/gRPC endpoint | http://localhost:4317  |

> If you stop the Jaeger container, rerun this command before testing traces again.

---

## Step 3 — Start SROx

Open **Terminal 3**, from the crate root:

```bash
cargo run
```

You should see:

```json
{"level":"INFO","fields":{"message":"SROx starting"}}
{"level":"INFO","fields":{"message":"listening","addr":"127.0.0.1:8443"}}
{"level":"INFO","fields":{"message":"metrics listening","addr":"127.0.0.1:9090"}}
```

If it exits immediately, check `config.toml` — the cert paths or upstream addr are likely wrong.

---

## Step 4 — Send requests through the proxy

Open **Terminal 4**.

**Single request — show response headers:**

```bash
curl -k -i https://127.0.0.1:8443/
```

Look for `X-Trace-Id` in the response headers. That confirms observability is wired end to end.

**Verbose — see everything including TLS handshake:**

```bash
curl -k -v https://127.0.0.1:8443/
```

**Test smuggling rejection:**

```bash
curl -k -i https://127.0.0.1:8443/ \
  -H "Content-Length: 5" \
  -H "Transfer-Encoding: chunked"
```

Expected: `HTTP/1.1 400 Bad Request`

**Burst — generate enough traffic to see metrics and traces:**

```bash
for i in {1..20}; do
  curl -k -s -o /dev/null https://127.0.0.1:8443/ &
done
wait
echo "burst complete"
```

**Health check path (if upstream has it):**

```bash
curl -k -i https://127.0.0.1:8443/healthz
```

---

## Step 5 — Check metrics

```bash
curl http://127.0.0.1:9090/metrics
```

**What to look for:**

```
# Active connections — should be 0 between requests
srox_active_connections 0

# Request duration histogram — populated after requests
srox_request_duration_seconds_bucket{method="GET",status_code="200",cache_status="MISS",le="0.005"} 14
srox_request_duration_seconds_count{method="GET",status_code="200",cache_status="MISS"} 20
srox_request_duration_seconds_sum{method="GET",status_code="200",cache_status="MISS"} 0.183
```

If `srox_request_duration_seconds_count` is 0 after sending requests, the histogram recording is not wired up in `serve_connection`.

If the metrics endpoint returns `curl: (1) Received HTTP/0.9 when not allowed` — the response headers in `metrics.rs` have a leading newline or extra whitespace. Fix the `format!` string to start with `HTTP/1.1` on the very first character.

---

## Step 6 — Check traces in Jaeger

1. Open **http://localhost:16686** in a browser
2. In the **Service** dropdown, select `srox-proxy`
3. Click **Find Traces**
4. Click any trace to see the full span tree

**What a healthy trace looks like:**

```
srox-proxy
└── connection  [trace_id=4bf92f...]
    ├── peer=127.0.0.1:52341
    ├── method=GET
    ├── path=/
    └── status=200
```

If no traces appear:

- Confirm Jaeger is still running (Terminal 2)
- Check `config.toml` — `otlp_endpoint` must be `"http://localhost:4317"`
- Check SROx logs for any `failed to build OTLP exporter` errors on startup

---

## Step 7 — Run benchmarks

Run the baseline (direct upstream) first, then through SROx. The difference between the two is your proxy overhead. Write both outputs to `bench/results/phase1b.txt`.

Pick **one** of the three options below depending on your environment.

---

### Option A — WSL (recommended if you are running Rust inside WSL)

Run this inside your WSL terminal:

```bash
# Install wrk
sudo apt install wrk

# Baseline — direct to upstream (no proxy)
wrk -t4 -c100 -d30s --latency http://localhost:8080/

# Through SROx
wrk -t4 -c100 -d30s --latency https://localhost:8443/
```

Expected output shape:

```
Latency Distribution
   50%    4.23ms
   75%    6.11ms
   90%    9.45ms
   99%   18.32ms
Requests/sec:   4823.11
```

---

### Option B — `bombardier` (native Windows binary, no install needed beyond download)

1. Download the latest `bombardier-windows-amd64.exe` from:
   https://github.com/codesenberg/bombardier/releases
2. Rename it to `bombardier.exe` and place it somewhere on your PATH (or run it from the download folder)

```powershell
# Baseline — direct to upstream (no proxy)
bombardier -c 100 -d 30s -l http://localhost:8080/

# Through SROx (skip TLS verification for self-signed cert)
bombardier -c 100 -d 30s -l -k https://localhost:8443/
```

Expected output shape:

```
Statistics        Avg      Stdev        Max
  Reqs/sec      4823.11    312.4      6021.3
  Latency        20ms       4ms       180ms
  Latency Distribution
     50%    18ms
     75%    22ms
     90%    28ms
     99%    61ms
```

---

### Option C — PowerShell (no install at all, built into Windows)

```powershell
# Baseline — direct to upstream (no proxy)
$times = 1..100 | ForEach-Object -Parallel {
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    try { Invoke-WebRequest -Uri "http://localhost:8080/" -UseBasicParsing | Out-Null } catch {}
    $sw.ElapsedMilliseconds
} -ThrottleLimit 20

"--- Baseline (direct upstream) ---"
"Count: $($times.Count)"
"Mean:  $([math]::Round(($times | Measure-Object -Average).Average, 2))ms"
"p99:   $(($times | Sort-Object)[[math]::Floor($times.Count * 0.99)])ms"

# Through SROx
$times = 1..100 | ForEach-Object -Parallel {
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    try { Invoke-WebRequest -Uri "https://localhost:8443/" -SkipCertificateCheck -UseBasicParsing | Out-Null } catch {}
    $sw.ElapsedMilliseconds
} -ThrottleLimit 20

"--- Through SROx proxy ---"
"Count: $($times.Count)"
"Mean:  $([math]::Round(($times | Measure-Object -Average).Average, 2))ms"
"p99:   $(($times | Sort-Object)[[math]::Floor($times.Count * 0.99)])ms"
```

> Option C gives you mean and p99 only — no histogram buckets. Good enough to see proxy overhead, but Options A or B give richer data.

---

### What to record

Whichever option you use, save the full output:

```bash
# WSL / bash
wrk -t4 -c100 -d30s --latency https://localhost:8443/ >> bench/results/phase1b.txt
```

```powershell
# PowerShell
bombardier -c 100 -d 30s -l -k https://localhost:8443/ | Out-File -Append bench/results/phase1b.txt
```

The numbers to pull out and put in `DESIGN.md`:

| Scenario                   | p50 | p99 | req/sec |
| -------------------------- | --- | --- | ------- |
| Direct upstream (baseline) | —   | —   | —       |
| Through SROx (Phase 1b)    | —   | —   | —       |

---

## Quick reference — what is running where

| Service         | Terminal | Address                         | Purpose                  |
| --------------- | -------- | ------------------------------- | ------------------------ |
| Python upstream | 1        | `http://localhost:8080`         | Upstream server          |
| Jaeger          | 2        | `http://localhost:16686`        | Trace UI                 |
| Jaeger OTLP     | 2        | `http://localhost:4317`         | Receives spans from SROx |
| SROx proxy      | 3        | `https://localhost:8443`        | Your proxy               |
| SROx metrics    | 3        | `http://localhost:9090/metrics` | Prometheus metrics       |

---

## Shutdown order

```bash
# Terminal 4 — nothing to stop
# Terminal 3 — Ctrl+C (SROx)
# Terminal 2 — Ctrl+C (Jaeger container stops and removes itself — --rm flag)
# Terminal 1 — Ctrl+C (Python server)
```
