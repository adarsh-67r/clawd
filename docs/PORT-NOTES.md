# Linux port notes

Engineering notes for the Wayland port. The design facts worth knowing when
you touch the sensors. (The pet is now a single native Rust binary — no
Tauri, no WebKit; history of the earlier Tauri draft lives in git.)

## Build

```bash
cargo build --release
./target/release/clawd
```

No webview or GTK dev packages needed. `cargo build --no-default-features`
builds the sensor layer without any window host (must stay green).

## Platform facts that shaped the pet rendering

- **Wayland host**: the pet is a `zwlr_layer_shell_v1` **Overlay** surface
  anchored bottom-right — it floats above tiled windows, never enters the
  window list, and never takes focus (`KeyboardInteractivity::None` jumps
  the focus back out on launch). Position and drag live in the anchored
  margins; walking is a margin swap, no compositor IPC.
- **wl_shm ARGB8888** is little-endian B,G,R,A — write RGBA and the pet
  renders with swapped channels.
- **Pointer events are only read via `prepare_read().read()`** in the poll
  loop; `dispatch_pending` alone never drains the socket (drag was dead for
  a session because of this).
- `wl_output.mode` flags arrive as `Current | Preferred` — accept with
  `contains`, not equality.

- **niri** (verified, 26.04):
  - IPC socket in `$NIRI_SOCKET`; newline-JSON protocol, no subprocess needed.
  - `event-stream` pushes focus + window-layout updates within ~100 ms.
  - Floating windows report `tile_pos_in_workspace_view`; tiled windows do
    not (upstream gap, niri-wm/niri#2381). Window following is therefore
    best-effort for floating windows, guaranteed only for a fixed perch.
  - Icons on taskbars/panels appear as `WindowOpenedOrChanged` events — the
    focus sensor tracks `WindowFocusChanged` + window titles.
- **Hyprland**: `socket2.sock` event stream with `activewindow>>app,title`
  lines; parser lives in `focus/hyprland.rs` and is unit-tested.
- **MPRIS**: any D-Bus player (Chromium/Firefox/Spotify expose it) — "Playing
  wins": whichever player is actively playing is what the pet reacts to.
- **UPower**: aggregate battery at
  `/org/freedesktop/UPower/devices/DisplayDevice` on the system bus.
- **Clipboard**: ext/wlr-data-control via wl-clipboard-rs; only MIME types
  are inspected, never content. The upstream "user pressed Ctrl+C" reaction
  becomes "clipboard changed" on Wayland — global key hooks would require a
  helper daemon, which this port refuses to spawn.
