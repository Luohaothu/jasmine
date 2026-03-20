# LAN Firewall Rules

## Required ports

- WebSocket: `9735` (local peer coordination and messaging API)
- UDP broadcast: `53317` (LAN discovery fallback)
- mDNS: `5353` (service discovery)

## Linux

### Quick checks

1. Ensure the runtime firewall allows inbound/outbound UDP + TCP on the ports above for your LAN interface.
2. If using `ufw`, allow ports:
   - `sudo ufw allow 9735/tcp`
   - `sudo ufw allow 53317/udp`
   - `sudo ufw allow 5353/udp`
   - `sudo ufw allow 5353/tcp` (some mDNS stacks use mixed transport)

### Notes

- Ports must be reachable between peers on the same subnet for discovery and transfer setup.
- If your network uses profile restrictions, apply these rules only on your trusted LAN profile.

## Windows

### Quick checks

1. Open **Windows Defender Firewall** → **Inbound Rules** → create inbound rules for:
   - `9735` TCP
   - `53317` UDP
   - `5353` UDP
2. Optionally create corresponding outbound rules for consistency on stricter profiles.
3. Use the app executable from the MSI/portable install when prompted for allow/block.

### Notes

- `5353` is commonly preused by mDNS. If your router blocks it, discovery may fall back to other mechanisms.
- Keep rules scoped to your home/work LAN when possible.

## Version and release checks

- After `pnpm tauri build`, verify generated Linux artifacts:
  - `src-tauri/target/release/bundle/deb/jasmine_0.1.0_amd64.deb`
  - `src-tauri/target/release/bundle/appimage/Jasmine_0.1.0_amd64.AppImage`
