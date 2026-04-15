# Windows target:
# Linux target (default):
# labbridger

A lightweight Actix-Web server for home-lab use that combines several features in one binary:

| Feature | Description |
|---------|-------------|
| **Network scanner** | Periodically pings every host on a configured subnet and tracks each device by IP + MAC address |
| **Target monitoring** | Background task probes every relay target (via HTTP or ICMP ping) and maintains a live `Offline → Starting → Online` status |
| **Remote start** | Wake devices via Wake-on-LAN or Shelly smart plugs |
| **Remote stop** | Gracefully shut down devices via SSH command, REST API call, and/or Shelly plug power-off — with a composite sequence that chains all three |
| **HTTP relay** | Proxies requests to named upstream targets, resolving MAC addresses to current IPs dynamically (safe for DHCP networks) |
| **Bearer-token auth** | Optional bcrypt-based login; all routes except `/login` and `/health` require a valid token |

TLS is intentionally omitted — HTTPS is expected to be handled upstream (e.g. Cloudflare Tunnel).

---

## Requirements

- Rust toolchain (edition 2024)
- Linux target for deployment (the network scanner reads `/proc/net/arp` and requires `CAP_NET_RAW` for raw ICMP)
- Cross-compilation toolchain if building on Windows for a Linux host (e.g. `aarch64-unknown-linux-gnu`)
- Shelly plug(s) on the same LAN, with their local HTTP API accessible (default on all Shelly devices)
- OpenSSH client on the server host (only required if using SSH-based remote stop)

---

## Configuration

Copy `config.yaml.example` to `config.yaml` and edit it before running (`config.yaml` is gitignored so your credentials stay local):

```yaml
server:
  host: "0.0.0.0"
  port: 8080
  token_ttl_seconds: 3600   # optional, default 3600
  # MAC addresses whose source IPs are allowed to skip bearer-token auth.
  # The server resolves peer IP → MAC via the system ARP cache.

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
    # MAC-based with Shelly power control (IP resolved dynamically):
    ollama:
      mac: "aa:bb:cc:dd:ee:ff"
      port: 11434
      probe_path: "/api/tags"                 # health check endpoint (optional, default: "/")
      shelly_power_mac: "11:22:33:44:55:66"   # MAC of Shelly plug controlling power

    # MAC-based with Wake-on-LAN support:
    wol_device:
      mac: "aa:bb:cc:dd:ee:ff"
      port: 8080
      wol_enabled: true

    # MAC-based with ping probe (useful for devices without HTTP service):
    ssh_device:
      mac: "cc:dd:ee:ff:11:22"
      port: 22
      probe_method: "ping"              # use ICMP ping instead of HTTP

    # MAC-based with SSH + Shelly shutdown (POST /stop/{name}):
    tv_box:
      mac: "cc:dd:ee:ff:11:22"
      port: 22
      probe_method: "ping"
      shelly_power_mac: "11:22:33:44:55:66"
      shutdown_plug_off: true                 # turn off plug after SSH shutdown
      shutdown_ssh:
        username: "root"
        # os: linux                       # target OS: linux (default) or windows
        # port: 22                        # SSH port (default: 22)
        # key_file: "/root/.ssh/id_ed25519"  # omit to use system default key
        # command: "sudo poweroff"         # auto-detected from os if omitted

    # MAC-based with SSH shutdown for a Windows server:
    win_server:
      mac: "cc:dd:ee:ff:33:44"
      port: 3389
      probe_method: "ping"
      shutdown_ssh:
        username: "Administrator"
        os: windows                       # uses "shutdown /s /t 0" by default

    # MAC-based with REST API shutdown (POST /stop/{name}):
    api_device:
      mac: "cc:dd:ee:ff:11:22"
      port: 8080
      shelly_power_mac: "11:22:33:44:55:66"
      shutdown_api_path: "/api/shutdown"  # POST to this path to shut down
      shutdown_plug_off: true             # turn off plug after API shutdown

    # Brute Shelly stop (just cut power, no graceful shutdown):
    dumb_device:
      mac: "cc:dd:ee:ff:11:22"
      port: 22
      probe_method: "ping"
      shelly_power_mac: "11:22:33:44:55:66"
      shutdown_plug_off: true             # sole stop method — just turn off plug

    # Static URL (always on, can't be remotely started/stopped):
    static_service: "http://192.168.1.50:8000"

    # Static URL with explicit probe path:
    managed_service:
      url: "http://192.168.1.60:9000"
      probe_path: "/health"

    # Static URL with ping probe:
    ping_monitored:
      url: "http://192.168.1.70:22"
      probe_method: "ping"
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
| `GET` | `/relays` | JSON map of all relay targets and their current status (`online`, `offline`, `starting`) with resolved IP |
| `POST` | `/start/{target}` | Start the named relay target via Wake-on-LAN or Shelly plug (if configured) |
| `POST` | `/stop/{target}` | Stop the named relay target via SSH, REST API, and/or Shelly plug power-off (fire-and-forget, returns `204`) |
| `*` | `/relay/{target}` | Proxy to the root of the named relay target |
| `*` | `/relay/{target}/{path}` | Proxy to `/{path}` on the named relay target (all methods, streaming body) |

---

## Target monitoring

A background task probes every configured relay target every 30 seconds and maintains its status:

| Status | Meaning |
|--------|---------|
| `offline` | Host not reachable — MAC absent from last scan (MAC-based), or service URL not responding (static) |
| `starting` | Host is on the network (MAC resolved) but the service is not yet responding to probes |
| `online` | Service endpoint is up and responding |

### Probe methods

Each target can be probed in one of two ways (set with `probe_method`):

- **`http`** (default) — sends a GET request to `probe_path` (default `"/"`); considers any response with status < 500 as success.
- **`ping`** — sends an ICMP ping; useful for devices that don't expose an HTTP service (e.g. SSH-only hosts).

The `/relays` endpoint returns the live status and resolved IP of every target.

---

## Smart plugs (Shelly)

Plugs are configured in `config.yaml` under `plugs`. Each entry maps a name to:

- `plug_ip` — the local IP of the Shelly device (assign a static DHCP lease to keep it stable)
- `target_mac` — the MAC address of the device the plug powers (used by `/wake` to know when it's online)

labbridger talks directly to the [Shelly Gen2 local HTTP/RPC API](https://shelly-api-docs.shelly.cloud/gen2/) — no cloud account needed. The following RPC calls are used:

- `Switch.GetStatus?id=0` — check whether the plug output is on or off
- `Switch.Set?id=0&on=true/false` — turn the plug on or off

When starting via Shelly, the server:
1. Checks if the powered device is already online (MAC visible in scan)
2. Checks the plug's current state — if it's already on but the device is offline, it power-cycles (off → 3 s delay → on)
3. Turns the plug on

---

## Wake-on-LAN

Devices can be remotely started using Wake-on-LAN by setting `wol_enabled: true` on a MAC-based relay target. When `POST /start/{target}` is called:

1. The server checks if the device is already online (MAC visible in the device list from the latest scan)
2. If offline, sends a Wake-on-LAN magic packet to the broadcast address (`<network_base>.255:9`)
3. Returns success once the packet is sent (the device may take 10-30 seconds to actually boot)

**Requirements:**
- The target device must support Wake-on-LAN and have it enabled in BIOS/firmware
- The device must be on the same subnet as the server
- Many devices require a wired Ethernet connection for WOL (Wi-Fi adapters often don't support it)

**Example configuration:**
```yaml
relay:
  targets:
    my_pc:
      mac: "aa:bb:cc:dd:ee:ff"
      port: 22
      wol_enabled: true
```

**Note:** If both `wol_enabled` and `shelly_power_mac` are set, Wake-on-LAN takes precedence.

---

## Remote stop

Devices can be remotely shut down via `POST /stop/{target}`. The endpoint is **fire-and-forget**: it spawns a background task and immediately returns **204 No Content**.

Three stop mechanisms are supported and can be combined:

### SSH shutdown (`shutdown_ssh`)

Runs a command on the remote device via the system OpenSSH client. The default command depends on the `os` field:

| `os`      | Default command      |
|-----------|----------------------|
| `linux`   | `sudo poweroff`      |
| `windows` | `shutdown /s /t 0`   |

**Authentication** — two modes are supported:

| Mode       | Config field | How it works |
|------------|-------------|--------------|
| **Key-based** (default) | `key_file` (optional) | Uses `BatchMode=yes`. If `key_file` is omitted the system default key is used. |
| **Password** | `password` | Uses [`sshpass`](https://linux.die.net/man/1/sshpass) to feed the password to OpenSSH. Install with `apt install sshpass`. The password is passed via environment variable, not the command line. |

```yaml
# Linux target – key-based auth (default):
shutdown_ssh:
  username: "root"
  os: linux                          # default
  port: 22                           # default: 22
  key_file: "/root/.ssh/id_ed25519"  # omit to use system default

# Windows target – password-based auth:
shutdown_ssh:
  username: "Administrator"
  os: windows
  password: "s3cret"                 # requires sshpass on the server
  # command: "shutdown /s /t 0"      # auto-detected from os if omitted
```

### REST API shutdown (`shutdown_api_path`)

Sends an HTTP POST to a path on the device (e.g. an application-level shutdown endpoint).

```yaml
shutdown_api_path: "/api/shutdown"
```

### Shelly plug power-off (`shutdown_plug_off`)

Turns off the Shelly smart plug that powers the device (set via `shelly_power_mac`). This can be used:

- **As the sole stop method** ("brute-force" power cut — no graceful shutdown)
- **After an SSH or REST API shutdown** (to fully de-power the device once it's offline)

Set `shutdown_plug_off: true` to enable. Defaults to `false` so that existing configs that only use `shelly_power_mac` for *starting* are not affected.

```yaml
# Brute Shelly stop (just cut power):
dumb_device:
  mac: "cc:dd:ee:ff:11:22"
  port: 22
  shelly_power_mac: "11:22:33:44:55:66"
  shutdown_plug_off: true

# SSH shutdown followed by plug off:
tv_box:
  mac: "cc:dd:ee:ff:11:22"
  port: 22
  shelly_power_mac: "11:22:33:44:55:66"
  shutdown_plug_off: true
  shutdown_ssh:
    username: "root"

# SSH shutdown without plug off (plug only used for /start):
server:
  mac: "cc:dd:ee:ff:11:22"
  port: 22
  shelly_power_mac: "11:22:33:44:55:66"
  # shutdown_plug_off: false          # default
  shutdown_ssh:
    username: "root"
```

### Composite stop sequence

When a target has multiple stop methods configured, the server runs them as a **composite sequence**:

1. **Graceful shutdown** — send SSH command *or* REST API request (SSH takes precedence if both are set)
2. **Wait for offline** — poll the ARP table until the device's MAC disappears (up to 5 minutes, checked every 10 s) — only when `shutdown_plug_off` is `true`
3. **Shelly plug off** — turn off the smart plug so the device is fully powered down — only when `shutdown_plug_off` is `true`

Errors in any step are logged but do **not** abort the remaining steps.

**Example — full composite stop:**
```yaml
relay:
  targets:
    tv_box:
      mac: "cc:dd:ee:ff:11:22"
      port: 22
      probe_method: "ping"
      shelly_power_mac: "11:22:33:44:55:66"
      shutdown_plug_off: true
      shutdown_ssh:
        username: "root"
```

Calling `POST /stop/tv_box` will: SSH into the device and run the OS-appropriate shutdown command (`sudo poweroff` for Linux, `shutdown /s /t 0` for Windows) → wait for the device to go offline → turn off the Shelly plug.

---

## Relay

Relay targets are defined in `config.yaml`. Three formats are supported:

- **MAC-based** — the scanner resolves the current IP at request time; safe when DHCP can reassign addresses.
- **Static URL shorthand** — a plain URL string, used as-is.
- **Static URL managed** — a URL with an explicit `probe_path` and/or `probe_method` for monitoring.

All request headers (except hop-by-hop headers) and the request body are forwarded. The response body is **streamed** so NDJSON / SSE streams (e.g. Ollama generate/chat) work without buffering.

---

## systemd deployment

A ready-to-use unit file is included:

```bash
# Copy binary and config
scp target/aarch64-unknown-linux-gnu/release/labbridger server@pi:/home/server/
scp config.yaml server@pi:/home/server/

# Install the service
scp rust-server.service server@pi:/etc/systemd/system/labbridger.service
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
  main.rs               – HTTP server, route wiring, auth middleware
  app_config.rs         – config.yaml schema (serde): Settings, RelayTarget, ProbeMethod, SshShutdownConfig
  auth.rs               – login/logout handlers, token store
  network_scanner.rs    – async ICMP scanner + ARP lookup
  relay.rs              – streaming HTTP reverse proxy
  relay_probe.rs        – target health probe implementations (HTTP GET / ICMP ping)
  target_monitor.rs     – background task that probes all relay targets on a 30 s loop
  target_status.rs      – TargetStatus enum (Offline / Starting / Online) and TargetInfo
  target_starter.rs     – POST /start/{target} handler (dispatches to WOL or Shelly)
  target_stopper.rs     – POST /stop/{target} handler + SshStoppable, RestApiStoppable, CompositeStoppable
  target_probeable.rs   – Probeable trait for polling target readiness
  target_startable.rs   – Startable trait for remote-start implementations
  target_stoppable.rs   – Stoppable trait for remote-stop implementations
  shelly.rs             – Shelly Gen2 smart plug control: ShellyStartable (power on) and ShellyPlugStoppable (power off)
  wol.rs                – Wake-on-LAN magic packet sender (WolStartable)
  bin/
    hash-password.rs    – CLI utility to bcrypt-hash a password
config.yaml             – runtime configuration (gitignored)
config.yaml.example     – safe template to copy from
docker-compose.yaml     – Starts mock-ollama container for development
rust-server.service     – systemd unit file
mock-ollama/            – Docker-based mock Ollama for testing relays
  Dockerfile
  mock_ollama.py
```
