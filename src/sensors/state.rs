// Platform-neutral state shape both Windows and Linux targets speak.
// (Extracted from Windows sensor code; Win32 impl bodies untouched.)

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DerivedState {
    pub mode: String,
    pub total_instances: u32,
    pub cli_count: u32,
    pub is_desktop_running: bool,
    pub focused_process: String,
    pub spotify_playing: bool,
    pub hour: u32,
    pub battery_pct: u32,
    pub is_charging: bool,
}
