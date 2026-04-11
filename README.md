# labbridger

A lightweight Actix-Web server for home-lab use that combines four things in one binary:

| Feature | Description |
|---------|-------------|
| **Network scanner** | Periodically pings every host on a configured subnet and tracks each device by IP + MAC address |
| **Smart plug control** | Turn Shelly Wi-Fi plugs on/off and wait for the powered device to come online |
| **HTTP relay** | Proxies requests to named upstream targets, resolving MAC addresses to current IPs dynamically (safe for DHCP networks) |
| **Bearer-token auth** | Optional bcrypt-based login; all routes except `/login` and `/health` require a valid token |

TLS is intentionally omitted — HTTPS is expected to be handled upstream (e.g. Cloudflare Tunnel).

---

## Requirements

- Rust toolchain (edition 2024)
- Linux target for deployment (the network scanner reads `/proc/net/arp` and requires `CAP_NET_RAW` for raw ICMP)
- Cross-compilation toolchain if building on Windows for a Linux host (e.g. `aarch64-unknown-linux-gnu`)
- Shelly plug(s) on the same LAN, with their local HTTP API accessible (default on all Shelly devices)

---

## Configuration

Copy `config.yaml.example` to `config.yaml` and edit it before running (`config.yaml` is gitignored so your credentials stay local):

```yaml
server:
  host: "0.0.0.0"
  port: 8080
  token_ttl_seconds: 3600   # optional, default 3600

scanner:
  network_base: "192.168.1" # scans .2 – .254
  delay_seconds: 30         # wait between passes

users:
  - username: "alice"
    password_hash: "$2b$12$..."   # generated with hash-password (see below)
  # leave the list empty to disable authentication entirely

# Shelly smart plugs + the device each plug powers
plugs:
  my-pc:
    plug_ip: "192.168.1.45"        # static IP of the Shelly plug
    target_mac: "aa:bb:cc:dd:ee:ff" # MAC of the device the plug powers

relay:
  targets:
    ollama:                       # reachable at /relay/ollama/...
      mac: "aa:bb:cc:dd:ee:ff"   # IP resolved from ARP table
      port: 11434
    other:                        # static URL alternative
      "http://192.168.1.50:8000"
```

### Generating a password hash

```bash
cargo run --bin hash-password -- mysecretpassword
```

Paste the printed hash into `config.yaml` under `password_hash`.

---

## Building

```bash
# Native debug build
cargo build

# Cross-compile for a Raspberry Pi (aarch64) – release
cargo build --release --target aarch64-unknown-linux-gnu
```

---

## Running

```bash
cargo run                      # debug, reads config.yaml in CWD
./target/release/labbridger     # release binary
```

The binary expects `config.yaml` in its working directory.

---

## API

### Public endpoints (no token required)

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/health` | Returns a simple HTML page confirming the server is up |
| `POST` | `/login` | Exchange credentials for a bearer token |

### `POST /login`

**Request body** (JSON):
```json
{ "username": "alice", "password": "mysecretpassword" }
```

**Response** (JSON):
```json
{ "token": "<64-char alphanumeric>", "expires_in": 3600 }
```

Send the token on every subsequent request:
```
Authorization: Bearer <token>
```

---

### Authenticated endpoints

| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/logout` | Immediately invalidates the current bearer token |
| `GET` | `/devices` | JSON map of all live devices `{ "<ip>": { "mac_address": "…", "discovered_at": "…" } }` |
| `GET` | `/devices/latest` | The single most recently discovered device, or `204 No Content` if no scan has completed yet |
| `POST` | `/plugs/{name}/on` | Turn the named Shelly plug on |
| `POST` | `/plugs/{name}/off` | Turn the named Shelly plug off |
| `GET` | `/plugs/{name}/status` | Get the current power state of the named Shelly plug |
| `POST` | `/plugs/{name}/wake` | Turn plug on and wait until the target device's MAC appears on the LAN, then return its IP |
| `*` | `/relay/{target}` | Proxy to the root of the named relay target |
| `*` | `/relay/{target}/{path}` | Proxy to `/{path}` on the named relay target (all methods, streaming body) |

---

## Smart plugs (Shelly)

Plugs are configured in `config.yaml` under `plugs`. Each entry maps a name to:

- `plug_ip` — the local IP of the Shelly device (assign a static DHCP lease to keep it stable)
- `target_mac` — the MAC address of the device the plug powers (used by `/wake` to know when it's online)

labbridger talks directly to the [Shelly local HTTP API](https://shelly-api-docs.shelly.cloud/gen1/) — no cloud account needed.

### Wake flow

`POST /plugs/{name}/wake`:

1. Sends `GET http://<plug_ip>/relay/0?turn=on` to power the device
2. Polls the network scanner every second until the `target_mac` appears in the ARP table
3. Returns `{ "ip": "192.168.1.x" }` once the device is online, or `504 Gateway Timeout` if it doesn't appear within the timeout (default 60 s)

---

## Relay

Relay targets are defined in `config.yaml`. Two formats are supported:

- **MAC-based** — the scanner resolves the current IP at request time; safe when DHCP can reassign addresses.
- **Static URL** — used as-is.

All request headers (except hop-by-hop headers) and the request body are forwarded. The response body is **streamed** so NDJSON / SSE streams (e.g. Ollama generate/chat) work without buffering.

---

## systemd deployment

A ready-to-use unit file is included:

```bash
# Copy binary and config
scp target/aarch64-unknown-linux-gnu/release/labbridger server@pi:/home/server/
scp config.yaml server@pi:/home/server/

# Install the service
scp labbridger.service server@pi:/etc/systemd/system/
ssh server@pi "sudo systemctl daemon-reload && sudo systemctl enable --now labbridger"

# View logs
ssh server@pi "journalctl -u labbridger -f"
```

The service runs as the `server` user with only `CAP_NET_RAW` (needed for ICMP pings).

---

## Mock Ollama (development)

A Python-based mock Ollama server is included for local testing of the relay:

```bash
docker compose up --build
```

This starts the mock on `localhost:11434`. Point a relay target at it in `config.yaml` to test streaming proxying without a real Ollama instance.

---

## Project structure

```
src/
  main.rs            – HTTP server, route wiring, auth middleware
  app_config.rs      – config.yaml schema (serde)
  auth.rs            – login/logout handlers, token store
  network_scanner.rs – async ICMP scanner + ARP lookup
  relay.rs           – streaming HTTP reverse proxy
  plugs.rs           – Shelly plug control + wake logic (planned)
  bin/
    hash-password.rs – CLI utility to bcrypt-hash a password
config.yaml          – runtime configuration (gitignored)
config.yaml.example  – safe template to copy from
rust-server.service  – systemd unit file (rename to labbridger.service)
mock-ollama/         – Docker-based mock for development
```
