// Linux sensor impls: /proc-based Claude detection, real config loading,
// compositor focus via the niri event-stream. The Windows impl
// stays untouched in system_state.rs/keypress.rs.

use super::battery;
use super::focus::SharedCache;
use super::mpris;
use super::state::DerivedState;
use super::proc::count_paths;
use chrono::Timelike;

#[cfg(test)]
mod tests {
    use super::*;
    use super::is_cli_focus;

    #[test]
    fn recognizes_claude_and_opencode_as_the_cli() {
        assert!(is_cli_focus("claude: Claude Code", "kitty"));
        assert!(is_cli_focus("kitty", "claude"));
        assert!(is_cli_focus("OC | Greeting", "kitty"), "opencode titles its terminal 'OC | …'");
        assert!(is_cli_focus("refactor", "Dev.sandbox-opencode"));
        assert!(!is_cli_focus("zsh in adarsh", "kitty"));
        assert!(!is_cli_focus("logos", "dev.duxlabs.files"));
    }
}

/// Focused-window rule: title/app identifies the AI CLI. The claude CLI
/// sets 'Claude Code' in titles; opencode titles terminals 'OC | …'.
fn is_cli_focus(title: &str, app: &str) -> bool {
    title.contains("Claude Code")
        || title.contains("OpenCode")
        || title.starts_with("OC |")
        || app == "claude"
        || app.contains("opencode")
}

pub fn poll_system_state(focus_cache: &SharedCache) -> DerivedState {
    let cli_count = count_paths("/proc", "claude") as u32 + count_paths("/proc", "opencode") as u32;
    let spotify_playing = mpris::read().playing;
    let focused = focus_cache.lock().unwrap().window.clone();
    let is_cli_focused = is_cli_focus(&focused.title, &focused.app);

    let (battery_pct, is_charging) = battery::read()
        .map(|b| (u32::from(b.percent), b.charging))
        .unwrap_or((100, true));

    DerivedState {
        mode: if is_cli_focused {
            "action".to_string()
        } else if cli_count > 0 {
            "awake".to_string()
        } else {
            "asleep".to_string()
        },
        total_instances: cli_count.max(1),
        cli_count,
        is_desktop_running: false,
        focused_process: focused.app,
        spotify_playing,
        hour: chrono::Local::now().hour(),
        battery_pct,
        is_charging,
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum KeyCommand {
    Copy,
    Paste,
    Screenshot,
}

/// Keyboard chords: real under X11 (XQueryKeymap), a no-op elsewhere —
/// Wayland already carries copy/paste through the clipboard watcher.
pub struct KeypressSensor {
    #[cfg(feature = "host-x11")]
    chords: Option<crate::sensors::x_chords::ChordSensor>,
}

#[cfg(feature = "host-x11")]
impl KeypressSensor {
    pub fn new() -> Self {
        if let (Some(_), None) = (
            std::env::var_os("DISPLAY"),
            std::env::var_os("WAYLAND_DISPLAY"),
        ) {
            if let Ok(c) = crate::sensors::x_chords::ChordSensor::new() {
                return Self { chords: Some(c) };
            }
        }
        Self { chords: None }
    }

    pub fn poll(&mut self) -> Option<KeyCommand> {
        self.chords.as_mut()?.poll()
    }
}

#[cfg(not(feature = "host-x11"))]
impl KeypressSensor {
    pub fn new() -> Self {
        Self {}
    }

    pub fn poll(&mut self) -> Option<KeyCommand> {
        None
    }
}


#[cfg(test)]
mod live_probe {
    use super::*;
    use crate::sensors::focus;

    /// Prints the derived state twice a second for 5s against the live
    /// session; shows what the pet's main loop actually receives.
    #[test]
    #[ignore = "live: prints poll_system_state against the real session"]
    fn probe_derived_state() {
        let cache = focus::new_cache();
        let c2 = cache.clone();
        std::thread::spawn(move || {
            focus::run_forever(focus::detect, &c2, std::time::Duration::from_secs(3));
        });
        for i in 0..10 {
            let st = poll_system_state(&cache);
            eprintln!("probe[{}] mode={} cli_focused_title={:?} cli_count={}", i, st.mode, {
                st.focused_process.clone()
            }, st.cli_count);
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    }
}
