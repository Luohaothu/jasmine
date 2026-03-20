## Task 0a: Tauri v2 + WebSocket Server Coexistence

### Approach
- Created an isolated spike app at `.sisyphus/tmp/task-0a/app` using `create-tauri-app` with the React + TypeScript template.
- Added `tokio`, `futures-util`, and `tokio-tungstenite` to `src-tauri/Cargo.toml`.
- Started a Tokio WebSocket echo server inside Tauri `setup` with `tauri::async_runtime::spawn`.
- Added a minimal frontend trigger in `src/App.tsx`:
  - A button opens `ws://127.0.0.1:9735`.
  - Sends JSON payload (default: `{\"type\":\"ping\",\"data\":\"hello\"}`).
  - Displays status and echoed response.

### Commands Run
- `mkdir -p .sisyphus/tmp/task-0a`
- `pnpm create tauri-app@latest app --template react-ts --manager pnpm --tauri-version 2 --yes`
- `pnpm install`
- `source ~/.cargo/env`
- `export HOMEBREW_PREFIX=/home/linuxbrew/.linuxbrew`
- `export PATH="$HOMEBREW_PREFIX/bin:$HOMEBREW_PREFIX/sbin:$PATH"`
- `export PKG_CONFIG_PATH="$HOMEBREW_PREFIX/opt/glib/lib/pkgconfig:$HOMEBREW_PREFIX/opt/gdk-pixbuf/lib/pkgconfig:$HOMEBREW_PREFIX/opt/cairo/lib/pkgconfig:$HOMEBREW_PREFIX/opt/pango/lib/pkgconfig:$HOMEBREW_PREFIX/opt/gtk+3/lib/pkgconfig:$HOMEBREW_PREFIX/opt/libsoup/lib/pkgconfig:$HOMEBREW_PREFIX/opt/webkitgtk/lib/pkgconfig:$HOMEBREW_PREFIX/opt/libayatana-appindicator/lib/pkgconfig:$HOMEBREW_PREFIX/opt/libdbusmenu/lib/pkgconfig:$HOMEBREW_PREFIX/lib/pkgconfig:$HOMEBREW_PREFIX/share/pkgconfig"`
- `export LD_LIBRARY_PATH="$HOMEBREW_PREFIX/lib:$HOMEBREW_PREFIX/opt/glib/lib:$HOMEBREW_PREFIX/opt/gtk+3/lib:$HOMEBREW_PREFIX/opt/webkitgtk/lib:$HOMEBREW_PREFIX/opt/libsoup/lib:$HOMEBREW_PREFIX/opt/cairo/lib:$HOMEBREW_PREFIX/opt/pango/lib:$HOMEBREW_PREFIX/opt/gdk-pixbuf/lib:$HOMEBREW_PREFIX/opt/libayatana-appindicator/lib:$HOMEBREW_PREFIX/opt/libdbusmenu/lib:${LD_LIBRARY_PATH:-}"`
- `export XDG_DATA_DIRS="$HOMEBREW_PREFIX/share:${XDG_DATA_DIRS:-/usr/local/share:/usr/share}"`
- `pnpm build`
- `cd src-tauri && cargo build`
- `xvfb-run -a pnpm tauri dev`
- `cargo --version`

### Result
- Frontend and Rust builds succeed in the isolated spike:
  - `pnpm build` succeeds (artifact generated).
  - `cargo build` in `src-tauri` succeeds with Linuxbrew native stack available.
- Headless Tauri launch (`xvfb-run -a pnpm tauri dev`) started and produced runtime output, but the captured transcript ends with `ELIFECYCLE Command failed.` (non-zero exit for the dev wrapper) after GTK/EGL/MESA warnings.
- UI and WS echo verification passed:
  - Tauri app launches to `http://localhost:1420/`.
  - Running `Run WS Echo` sends `{"type":"ping","data":"hello"}` to `ws://127.0.0.1:9735` and receives an echo containing `"hello"`.
- Responsiveness check passed:
  - 100 rapid WS messages in the same app session succeeded (`success=100`, `failures=0`, `elapsed_ms=56.4`).
- Evidence files written:
  - `.sisyphus/evidence/task-0a-pnpm-build.txt`
  - `.sisyphus/evidence/task-0a-tauri-dev.txt`
  - `.sisyphus/evidence/task-0a-cargo-check.txt`
  - `.sisyphus/evidence/task-0a-cargo-build.txt`
  - `.sisyphus/evidence/task-0a-ws-echo.txt`
  - `.sisyphus/evidence/task-0a-responsiveness.txt`

### Task 0a (Linuxbrew Retry) Evidence
- `task-0a-ws-echo.txt`: PASS (`{"type":"ping","data":"hello"}` echoed)
- `task-0a-responsiveness.txt`: PASS (`success=100`, `failures=0`)
- `task-0a-cargo-build.txt`: PASS (`Finished dev profile`)
- `task-0a-tauri-dev.txt`: app startup and runtime warnings captured; transcript ends with `ELIFECYCLE Command failed.`

### Observed Caveats
- Runtime uses Linuxbrew GTK/WebKit runtime with `xvfb-run`; non-fatal runtime warnings appear in logs (`Gtk-CRITICAL`, `libEGL`, `MESA`), but app stays responsive and functionality under test passed.
- If stricter desktop integration is required, consider using a full desktop GL environment for warning hygiene.

### Verdict
- `YES` for functional Task 0a validation (WS echo + responsiveness checks passed from captured evidence).
- `NO` for clean `pnpm tauri dev` transcript in headless execution (`ELIFECYCLE Command failed.` remains in evidence and should be interpreted separately).
- `YES` for isolated spike scaffolding and code-level integration pattern for Tauri setup + ws echo server.
