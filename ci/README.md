# npt-ci — zf-worker forwarding stress / continuity CI

Automated test that drives traffic through zf-worker **forward ports** and flags
**stalls (断流)** and **mid-stream disconnects** — the core forwarding breakage
classes. One self-contained binary, runs on any machine.

## Topology (Mode A)

```
npt-ci run  ──connect ip:port──▶  [zf-worker forward port]  ──forward──▶  npt-ci serve (echo)
   (CI runner)                                                              (forward target)
```

The CI runner resolves each port's public `ip:port` from the panel API, opens N
concurrent flows, streams framed payloads, and verifies they come back
continuously from the echo endpoint. Because the test target is the controlled
`npt-ci serve` echo, stall detection is exact.

> Current version supports **raw TCP/UDP forward ports** (no inbound proxy
> handshake). Ports with an inbound proxy (SS/vless/hammer/ssh/wireguard) are
> reported as `SKIP`.

## Setup

1. **Deploy the echo endpoint** at the forward target host:
   ```bash
   npt-ci serve --listen 0.0.0.0:9000        # add --no-udp to disable UDP echo
   ```
2. **Configure forwarding** (in the panel) so the ports under test forward to
   `<echo-host>:9000`. (You said this is already done.)
3. **Create an API key** in the panel. Use an **admin-owned** key if you want
   failure runs to auto-pull worker debug logs (`collect_logs = true`).
4. Copy `ci.example.toml` → `ci.toml`, fill in `panel_url`, `api_key`, `port_ids`.

## Run

```bash
npt-ci run -c ci.toml
# overrides:
npt-ci run -c ci.toml --ports 101,102 --duration 60 --connections 64
```

Exit code is non-zero if any port fails — drop it straight into a CI job.

## What counts as a failure

Per port (configurable in `[detect]`):

- **stall** — an active flow received no echoed bytes for `stall_secs` (断流)
- **one-way dead (单通)** — a flow connected but got zero bytes echoed back the
  whole test (e.g. inbound firewall reject, or upstream accepts then never relays)
- **mid-stream disconnect** — a flow hit EOF/RST before the test window ended
- **no connection succeeded** — port effectively down
- connect-fail ratio over limit; optional throughput floor / p95 RTT ceiling

> **Operational note:** zf-worker forward ports sit behind an **inbound firewall
> with a default-reject policy**. The machine running `npt-ci run` must have its
> source IP allowlisted, otherwise every flow is rejected and shows up as
> `one-way dead (单通)` / mid-stream disconnect. The archived `blocked_ips.json`
> snapshot names the rejected source IP and reason — check it first when a whole
> port fails with zero return traffic.

## Artifacts

Each run writes `runs/run-<timestamp>/summary.json` plus a console table. On a
port failure it also creates `port-<id>-<proto>/` containing:

- `connection_stats.json`, `test_latency.json`, `blocked_ips.json` — panel snapshots
- `server-<id>-<name>-debug.zip` — worker diagnostic bundle (admin key only)

## Test

```bash
cargo test -p npt-ci      # echo_roundtrip_is_clean: real loopback data-plane check
```
