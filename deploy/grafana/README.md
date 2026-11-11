# Server diagnostics

Import `socialspeak.json` in Grafana and choose your Prometheus datasource.
The dashboard uses the server's existing T-032 metrics; no client telemetry or
per-user labels are added. Each time series preserves the `instance` label.
Rates use a five-minute window. DB latency is writer-lock time, not end-to-end
request latency; low traffic may yield no percentile rather than a zero.

Scrape the internal listener from a Prometheus agent on the server host:

```yaml
scrape_configs:
  - job_name: socialspeak
    scrape_interval: 10s
    static_configs:
      - targets: ['127.0.0.1:9101']
```

Keep the internal listener on loopback. Do not publish it through the public
reverse proxy or container ports. A containerized Prometheus needs an explicitly
configured host-local agent/network path; its own localhost is not the server.

Inspect individual live QUIC paths on the server host:

```sh
ss-server conn list --data-dir ./data
# Docker Compose deployment:
docker compose -f deploy/docker-compose.yml exec socialspeak /ss-server conn list --data-dir /data
```

The command loads the same config/env overrides as `run` and reads only
`GET /connections` on the internal listener, bypassing proxies and
redirects with a five-second timeout. A loopback bind is used as is; a
wildcard bind such as the container's `SS_INTERNAL_BIND=0.0.0.0:9101` is
reached on `127.0.0.1` (or `::1`) inside the same host or container. Any
other address is refused, so the request never leaves the machine. The
Compose healthcheck runs this same command. Output is JSON: ephemeral connection ID,
RTT in milliseconds, cumulative path packets sent/lost, congestion window and
UDP payload MTU in bytes, and pending control-message count. It omits addresses,
accounts, auth tokens and client-supplied strings. Counters reset with a new
connection/path. The server must be running; failures exit non-zero.

Dashboard rendering needs a running Grafana/Prometheus deployment; JSON and
metric-name checks do not establish a production deployment acceptance result.
