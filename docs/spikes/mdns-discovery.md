# mdns-sd Spike: `_jasmine._tcp.local` Self-Discovery

## Goal
Validate `mdns-sd` can register and browse the Jasmine service type on Linux as a non-root user, and record evidence.

## Approach
- Used a throwaway Rust binary under `.sisyphus/tmp/task-0b/mdns_spike/`.
- Added dependency: `mdns-sd = "0.18"`.
- In-process flow:
  - Create `ServiceDaemon`.
  - Register service type `_jasmine._tcp.local.` with TXT values:
    - `device_id=test-001`
    - `display_name=TestDevice`
    - `ws_port=9735`
  - Browse `_jasmine._tcp.local.` and listen for `ServiceEvent` messages.
  - Print resolved TXT values and stop when expected values are confirmed.
- Ensured runtime user was not `root`.

## Exact commands
```bash
source ~/.cargo/env
cd .sisyphus/tmp/task-0b/mdns_spike
cargo run --quiet
```

Evidence snapshots:
- `.sisyphus/evidence/task-0b-whoami.txt`
- `.sisyphus/evidence/task-0b-mdns-discovery.txt`
- `.sisyphus/evidence/task-0b-mdns-no-root.txt`

## Observed output (Linux)
```text
whoami: leo
command: cargo run --quiet
Registered service and started browse for _jasmine._tcp.local.
Event: SearchStarted("_jasmine._tcp.local. on 3 interfaces [...])
Event: ServiceFound("_jasmine._tcp.local.", "test-device._jasmine._tcp.local.")
Discovered service: _jasmine._tcp.local.
Discovered instance: test-device._jasmine._tcp.local.
host: 127.0.0.1.local.
port: 9735
txt: device_id=test-001 display_name=TestDevice ws_port=9735
Self discovery verification passed
Finished: elapsed 0s
```

## Linux validation result
- PASS: mDNS registration + browse works.
- PASS: service is discovered within the spike run window.
- PASS: TXT records include exact required values.
- PASS: ran as regular user (`leo`), no permission-denied errors.

## Cross-platform note
`mdns-sd` docs show Linux/Windows/macOS target support and RFC basis (`RFC 6762`, `RFC 6763`), which is a good signal for Windows compatibility, but this environment only executed a Linux runtime verification. On Windows, follow docs/Bonjour guidance in environment docs before shipping runtime expectations.
