#!/usr/bin/env bash
set -e

PROXY="https://localhost:8443"
UPSTREAM="http://localhost:8080"
DURATION="30s"
THREADS=4
CONNECTIONS=100
PHASE=${1:-"unknown"}

echo "=== SROx Benchmark — Phase $PHASE ==="
echo "Date: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
echo ""

echo "--- Direct upstream (baseline) ---"
wrk -t$THREADS -c$CONNECTIONS -d$DURATION --latency $UPSTREAM

echo ""
echo "--- Through SROx proxy ---"
wrk -t$THREADS -c$CONNECTIONS -d$DURATION --latency $PROXY

echo ""
echo "--- Prometheus metrics snapshot ---"
curl -s http://localhost:9090/metrics | grep -E "srox_request_duration|srox_active"