// Native pet entry: sensors feed the pose engine, the pose engine feeds the
// window host. Three sensor threads (system state, clipboard, keypad) push
// Signals over one channel; the main loop owns the host, pet and clock.

pub mod host;
mod gfx;
pub mod pet;
mod sensors;

use gfx::{blit_scale, Canvas, Sheet, PAD};
use host::{AppEvent, Host};
use pet::Pet;
use sensors::proc::Config;
use sensors::state::DerivedState;
use chrono::Timelike;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_candidate_with_idle_art_wins() {
        let a = PathBuf::from("/nonexistent/a");
        let b = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("media");
        let c = PathBuf::from("/nonexistent/c");
        let got = find_media_dir(&[a, c.clone(), b.clone()]);
        assert_eq!(got, Some(b), "order wins, but a dir without idle.png never does");
        assert_eq!(find_media_dir(&[c]), None);
    }
}

/// First candidate dir that actually contains the idle art (parity with
/// Config::load: XDG first, then exe-adjacent, then cwd).
fn find_media_dir(candidates: &[PathBuf]) -> Option<PathBuf> {
    candidates.iter().find(|d| d.join("idle.png").is_file()).cloned()
}

fn media_candidates() -> Vec<PathBuf> {
    let xdg_data = match std::env::var("XDG_DATA_HOME") {
        Ok(v) if !v.is_empty() => PathBuf::from(v).join("clawd/media"),
        _ => PathBuf::from(
            std::env::var("HOME").map(|h| h + "/.local/share/clawd/media").unwrap_or_default(),
        ),
    };
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()));
    [
        Some(xdg_data),
        exe_dir.as_ref().map(|d| d.join("media")),
        exe_dir.as_ref().map(|d| d.join("..").join("media")),
        Some(PathBuf::from("media")),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// What the sensor threads push toward the main loop.
enum Signal {
    State(DerivedState),
    Clipboard(sensors::clipboard::ClipboardKind),
    Key(sensors::KeyCommand),
    /// global cursor position (X11 eye-follow)
    Cursor(i32, i32),
}

fn main() {
    let cfg = sensors::proc::Config::load();
    eprintln!("clawd: config loaded");
    let media = find_media_dir(&media_candidates()).unwrap_or_else(|| {
        eprintln!("clawd: media/ not found next to the exe, in XDG data, or cwd");
        std::process::exit(1);
    });
    let sheet = Sheet::load(&media);
    let (tx, rx) = channel::<Signal>();
    spawn_sensor_threads(&tx, &cfg);
    if let Err(e) = run(cfg, &sheet, rx) {
        eprintln!("clawd: host failed: {e}");
        std::process::exit(1);
    }
}

fn spawn_sensor_threads(tx: &Sender<Signal>, cfg: &Config) {
    // system state + focus event stream (upstream cadences)
    let tx_state = tx.clone();
    let system_ms = Duration::from_millis(cfg.raw["poll"]["systemState"].as_u64().unwrap_or(2500));
    std::thread::spawn(move || {
        let cache = sensors::focus::new_cache();
        let c2 = cache.clone();
        std::thread::spawn(move || {
            sensors::focus::run_forever(sensors::focus::detect, &c2, Duration::from_secs(3));
        });
        loop {
            let state = sensors::poll_system_state(&cache);
            if tx_state.send(Signal::State(state)).is_err() {
                break;
            }
            std::thread::sleep(system_ms);
        }
    });

    // clipboard watcher: Wayland's copy reaction source
    let tx_clip = tx.clone();
    std::thread::spawn(move || {
        sensors::clipboard::watch(move |kind| {
            if matches!(
                kind,
                sensors::clipboard::ClipboardKind::Text | sensors::clipboard::ClipboardKind::Image
            ) {
                let _ = tx_clip.send(Signal::Clipboard(kind));
            }
        });
    });

    // keypress sensor: X11 chords (ctrl+c/v, Print, super+shift+s); no-op
    // elsewhere — Wayland copy arrives via the clipboard watcher instead
    let tx_key = tx.clone();
    let key_ms = Duration::from_millis(cfg.raw["poll"]["keypress"].as_u64().unwrap_or(80));
    std::thread::spawn(move || {
        let mut sensor = sensors::KeypressSensor::new();
        loop {
            if let Some(cmd) = sensor.poll() {
                if tx_key.send(Signal::Key(cmd)).is_err() {
                    break;
                }
            }
            std::thread::sleep(key_ms);
        }
    });

    // X11-only global cursor poll feeds the eyes (Wayland has no global
    // cursor poll; there the eyes follow only while hovering the pet)
    #[cfg(feature = "host-x11")]
    if let (Some(_), None) = (
        std::env::var_os("DISPLAY"),
        std::env::var_os("WAYLAND_DISPLAY"),
    ) {
        let tx_cur = tx.clone();
        let cur_ms = Duration::from_millis(cfg.raw["poll"]["cursor"].as_u64().unwrap_or(33));
        std::thread::spawn(move || {
            let Ok(mut sensor) = sensors::x_cursor::CursorSensor::new() else {
                return;
            };
            loop {
                if let Some((x, y)) = sensor.poll() {
                    if tx_cur.send(Signal::Cursor(x, y)).is_err() {
                        break;
                    }
                }
                std::thread::sleep(cur_ms);
            }
        });
    }
}

fn run(cfg: Config, sheet: &Sheet, rx: Receiver<Signal>) -> Result<(), String> {
    let mut host = host::open()?;
    eprintln!("clawd: host ready");
    let mut pet = Pet::new(&cfg);
    let (fw, fh) = host.size();
    let mut canvas = Canvas::new(fw, fh);

    let start = Instant::now();
    let frame_ms = Duration::from_millis(cfg.raw["poll"]["cursor"].as_u64().unwrap_or(33));

    // sensors haven't answered yet; match upstream's power-on defaults
    let mut last_state = DerivedState {
        mode: "awake".into(),
        total_instances: 1,
        cli_count: 0,
        is_desktop_running: false,
        focused_process: String::new(),
        spotify_playing: false,
        hour: chrono::Local::now().hour() as u32,
        battery_pct: 100,
        is_charging: true,
    };
    pet.apply(&last_state);

    loop {
        // 1: drain sensor signals — latest state wins, reactions accumulate
        loop {
            match rx.try_recv() {
                Ok(Signal::State(st)) => {
                    last_state = st;
                    pet.apply(&last_state);
                }
                Ok(Signal::Clipboard(_)) => pet.on_copy(start.elapsed().as_millis() as u64),
                Ok(Signal::Key(cmd)) => {
                    let now = start.elapsed().as_millis() as u64;
                    match cmd {
                        sensors::KeyCommand::Copy => pet.on_copy(now),
                        sensors::KeyCommand::Paste => pet.on_paste(now),
                        sensors::KeyCommand::Screenshot => pet.on_screenshot(now),
                    }
                }
                Ok(Signal::Cursor(gx, gy)) => {
                    // global → pet-window coords for the eyes
                    let (px, py) = host.position();
                    if px != 0 || py != 0 {
                        pet.aim_eyes((gx - px) as f64, (gy - py) as f64);
                    }
                }
                Err(_) => break,
            }
        }

        // 2: host input
        for ev in host.poll_events() {
            match ev {
                AppEvent::Click => {
                    pet.on_click(start.elapsed().as_millis() as u64);
                    // upstream: petted pet opens Claude when it isn't running
                    if !last_state.is_desktop_running {
                        spawn_claude(&cfg.claude_command);
                    }
                }
                AppEvent::Dragged => pet.on_drag(),
                AppEvent::Cursor(sx, sy) => pet.aim_eyes(sx, sy),
                AppEvent::Quit => return Ok(()), // clean exit on a quit event
                _ => {}
            }
        }

        // walk + pose this frame (position lives in the host's margins)
        let now = start.elapsed().as_millis() as u64;
        let (sw, sh) = host.screen_size();
        pet.set_screen_size(sw, sh);
        let (px, py) = host.position();
        pet.sync_pos(px, py);
        let pose = pet.tick(now);
        if pet.walking() {
            let (x, y) = pet.pos();
            let _ = host.move_to(x, y);
        }

        // 4: the pet always owns its own pixels (input region = opaque art):
        // the art region stays live for drag + pet-under-cursor semantics.
        host.set_passthrough(false);

        // 5: draw (typing animates inside the pose engine, not the blit)
        if let Some(name) = pose {
            if let Some(frame) = sheet.get(name) {
                canvas = Canvas::new(fw, fh);
                blit_scale(&mut canvas, frame, PAD as i32, PAD as i32, 1, pet.eye_dx(), pet.bounce_for(name, now));

                host.present(&canvas);
            }
        }

        std::thread::sleep(frame_ms);
    }
}

fn spawn_claude(command: &str) {
    let mut parts = command.split_whitespace();
    let Some(bin) = parts.next() else { return };
    let _ = std::process::Command::new(bin).args(&mut parts).spawn();
}
