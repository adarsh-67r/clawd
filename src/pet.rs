use crate::gfx::WINDOW_W;
use crate::sensors::proc::Config;
use crate::sensors::state::DerivedState;

// Reaction lengths / phase durations (upstream verbatim).
const DROWSY_MS: u64 = 4500;
const HAPPY_MS: u64 = 4000;
const CHEESE_MS: u64 = 2500;
const CHARGING_MS: u64 = 3000;
const FIRST_COFFEE_AT: u64 = 8000;
const COFFEE_INTERVAL: u64 = 18000;
const COFFEE_LEN: u64 = 2500 + 1000 + 1200 + 1500 + 800;
const COPY_LEN: u64 = 350 + 2000 + 350;
const PASTE_LEN: u64 = 350 + 1800 + 350;
const LOW_BATTERY_PCT: u32 = 20;

// eye tracking (renderer.js): eyes slide at most 1px toward the cursor
// within a 150px radius, only while the pet is resting awake
const EYE_CENTER: (f64, f64) = (
    crate::gfx::PAD as f64 + 190.0 * (7.0 / 15.0),
    crate::gfx::PAD as f64 + 200.0 * (9.0 / 16.0),
);
const EYE_RADIUS: f64 = 150.0;

// random walk (renderer.js verbatim)
const WALK_CHECK_MS: u64 = 9000;
const WALK_AFTER_CYCLES: u32 = 8;
const WALK_SETTLE_MS: u64 = 200;

/// The derived open-stride twin of a walk pose (registered at load by gfx).
fn walk_swing(dir: &'static str) -> &'static str {
    if dir == "walk-right" { "walk-right-swing" } else { "walk-left-swing" }
}

// awake idle cycle: rest/look/rest/look/rest/scratch/rest/thug/rest/rest —
// durations from renderer.js IDLE_PHASES (thug on/pose/off share one art).
const IDLE_PHASES: &[(&'static str, u64)] = &[
    ("idle", 4000),
    ("look-right", 2000),
    ("idle", 3000),
    ("look-left", 2000),
    ("idle", 5000),
    ("scratch", 1800),
    ("idle", 3000),
    ("thuglife", 3000),
    ("idle", 500),
    ("idle", 2000),
];

const LAZY_START: u64 = u64::MAX;

enum Reaction {
    Happy { start: u64 },
    Cheese { start: u64 },
    Charging { start: u64 },
    Copy { start: u64 },
    Paste { start: u64 },
}

enum Mode {
    Awake,
    Action,
    Drowsy,
    Asleep,
}

pub struct Pet {
    mode: Mode,
    cli_count: u32,
    last_state_mode: String,
    reaction: Option<Reaction>,
    phase_index: usize,
    phase_anchor: Option<u64>,
    was_charging: bool,
    spotify: bool,
    hour: u32,
    battery_pct: u32,
    charging: bool,
    reset_phases: bool,
    next_coffee: u64,
    coffee_until: Option<u64>,
    // position + walk state (screen coords; the picture window is bottom-anchored)
    pos_x: i32,
    pos_y: i32,
    screen_w: i32,
    walk: Option<Walk>,
    next_walk_check: u64,
    walk_cycles: u32,
    // eye-follow offset in px, refreshed while the cursor moves near the pet
    eye_dx: i32,
}

/// One commanded walk: linear dock from x0 to target at the upstream pace
/// (1.5px per 16ms), then a short settle before idle resumes.
enum Walk {
    Moving { start: u64, x0: i32, target: i32, duration: u64, right: bool },
    Settling { until: u64, right: bool },
}


impl Reaction {
    fn frame(&self, now: u64) -> Option<&'static str> {
        match *self {
            Reaction::Happy { start } if now < start + HAPPY_MS => Some("happy"),
            Reaction::Cheese { start } if now < start + CHEESE_MS => Some("cheese"),
            Reaction::Charging { start } if now < start + CHARGING_MS => Some("charging"),
            Reaction::Copy { start } if now < start + COPY_LEN => Some("copy"),
            Reaction::Paste { start } if now < start + PASTE_LEN => Some("paste"),
            _ => None,
        }
    }
}

impl Pet {
    // shortcut: config timing constants are house consts, not read from _cfg
    // (pinned by unit tests as upstream verbatim); switch when a user wants tuning
    pub fn new(_cfg: &Config) -> Pet {
        Pet {
            mode: Mode::Awake,
            cli_count: 1,
            last_state_mode: "awake".into(),
            reaction: None,
            phase_index: 0,
            phase_anchor: None,
            was_charging: true, // start like the renderer inits (charging), so only a real edge fires
            spotify: false,
            hour: 12,
            battery_pct: 100,
            charging: true,
            reset_phases: false,
            next_coffee: LAZY_START,
            coffee_until: None,
            pos_x: 0,
            pos_y: 0,
            screen_w: 0,
            walk: None,
            next_walk_check: LAZY_START,
            walk_cycles: 0,
            eye_dx: 0,
        }
    }

    pub fn apply(&mut self, st: &DerivedState) {
        self.cli_count = st.cli_count;
        self.spotify = st.spotify_playing;
        self.hour = st.hour;
        self.battery_pct = st.battery_pct;
        self.charging = st.is_charging;

        // upstream walkStep aborts the walk the moment the mode leaves awake
        if st.mode != "awake" {
            self.walk = None;
        }

        // charging celebration is an edge (upstream wasCharging), like the
        // renderer's doChargingCelebration
        if st.is_charging && !self.was_charging && self.reaction.is_none() {
            self.reaction = Some(Reaction::Charging { start: LAZY_START });
        }
        self.was_charging = st.is_charging;

        let new_mode = st.mode.as_str();
        if new_mode != self.last_state_mode {
            self.reset_phases = true;
            self.last_state_mode = new_mode.to_string();
            if new_mode == "action" {
                // entering action re-arms the coffee relay (upstream startCoffeeBreaks)
                self.next_coffee = LAZY_START;
                self.coffee_until = None;
            }
        }

        self.mode = match new_mode {
            "action" => Mode::Action,
            "asleep" => Mode::Drowsy, // asleep is reached via the 4500ms drowsy timer in tick
            _ => Mode::Awake,
        };
    }

    pub fn on_copy(&mut self, now: u64) {
        // upstream gate (canClipboardReact): awake only; clipboard events carry
        // MIME presence only — no key hooks on Wayland (design decision)
        if self.reaction.is_none() && matches!(self.mode, Mode::Awake) {
            self.reaction = Some(Reaction::Copy { start: now });
        }
    }

    pub fn on_paste(&mut self, now: u64) {
        if self.reaction.is_none() && matches!(self.mode, Mode::Awake) {
            self.reaction = Some(Reaction::Paste { start: now });
        }
    }

    pub fn on_click(&mut self, now: u64) {
        if self.walk.is_some() {
            return; // upstream: petting is skipped while walking
        }
        if self.reaction.is_none() {
            self.reaction = Some(Reaction::Happy { start: now });
        }
    }

    /// True while a reaction is on screen — the window is clickable then and
    /// (config permitting) click-through otherwise.
    pub fn reactive(&self) -> bool {
        self.reaction.is_some()
    }

    pub fn on_screenshot(&mut self, now: u64) {
        if self.reaction.is_none() {
            self.reaction = Some(Reaction::Cheese { start: now });
        }
    }

    // ---- walk plumbing (screen coords; the host carves these from margins) ----

    pub fn set_screen_size(&mut self, w: u32, _h: u32) {
        self.screen_w = w as i32;
    }

    /// Read back the window position; ignored mid-walk so the walk clock wins.
    pub fn sync_pos(&mut self, x: i32, y: i32) {
        if self.walk.is_none() {
            self.pos_x = x;
            self.pos_y = y;
        }
    }

    pub fn pos(&self) -> (i32, i32) {
        (self.pos_x, self.pos_y)
    }

    pub fn walking(&self) -> bool {
        self.walk.is_some()
    }

    /// A drag physically moved the window: drop any walk in progress.
    pub fn on_drag(&mut self) {
        self.walk = None;
    }

    // ---- eye tracking ----

    /// Feed a cursor hit; coords are pet-window-space (callers translate).
    pub fn aim_eyes(&mut self, cx: f64, cy: f64) {
        let dx = cx - EYE_CENTER.0;
        let dy = cy - EYE_CENTER.1;
        let dist = (dx * dx + dy * dy).sqrt().max(1.0);
        let clamped = dist.min(EYE_RADIUS);
        self.eye_dx = ((dx / dist) * (clamped / EYE_RADIUS)).round() as i32;
    }

    /// Vertical hop for the sudareflen art (renderer.css walk-bob 0.4s,
    /// vibe-bob 0.6s; bitmap stand-in for the SVG pivots). 0 elsewhere.
    /// Only counts while actually moving — the settle holds still.
    pub fn bounce_for(&self, pose: &str, now: u64) -> i32 {
        match pose {
            "walk-right" | "walk-left" | "walk-right-swing" | "walk-left-swing" => {
                if matches!(self.walk, Some(Walk::Moving { .. })) && now % 400 < 200 {
                    -1
                } else {
                    0
                }
            }
            "vibing" => {
                if now % 600 < 300 { -1 } else { 0 }
            }
            _ => 0,
        }
    }

    /// Active only while the pet is resting awake (upstream gate).
    pub fn eye_dx(&self) -> i32 {
        if !matches!(self.mode, Mode::Awake)
            || self.reaction.is_some()
            || self.walk.is_some()
            || self.spotify
        {
            return 0;
        }
        let phase = IDLE_PHASES[self.phase_index.min(IDLE_PHASES.len() - 1)].0;
        if phase != "idle" {
            return 0;
        }
        self.eye_dx
    }

    fn walk_frame(&mut self, now: u64) -> Option<&'static str> {
        match self.walk {
            None => None,
            Some(Walk::Moving { start, x0, target, duration, right }) => {
                let elapsed = now.saturating_sub(start);
                if elapsed >= duration {
                    self.pos_x = target;
                    let pose = if right { "walk-right" } else { "walk-left" };
                    self.walk =
                        Some(Walk::Settling { until: start + duration + WALK_SETTLE_MS, right });
                    Some(pose)
                } else {
                    let dx = (target - x0) as i64;
                    self.pos_x =
                        x0 + ((dx * elapsed as i64) / duration as i64) as i32;
                    // stride: base (legs together) and derived swing (legs
                    // open) alternate on a 0.4s cycle, matching walk-bob
                    let dir = if right { "walk-right" } else { "walk-left" };
                    Some(if now % 400 < 200 { dir } else { walk_swing(dir) })
                }
            }
            Some(Walk::Settling { until, right }) => {
                if now >= until {
                    self.walk = None;
                    None
                } else {
                    Some(if right { "walk-right" } else { "walk-left" })
                }
            }
        }
    }

    /// Upstream doWalk: 1.5px per 16ms at the (possibly wall-clamped) target.
    fn begin_walk(&mut self, now: u64, right: bool, dist: i32) {
        let raw = if right { self.pos_x + dist } else { self.pos_x - dist };
        let target = raw.clamp(0, self.screen_w - WINDOW_W as i32);
        let dx = (target - self.pos_x).abs();
        let duration = (dx as u64 * 32 + 2) / 3; // dx/1.5 steps * 16ms
        self.walk = Some(Walk::Moving { start: now, x0: self.pos_x, target, duration, right });
    }

    /// walkChance 0.3, direction + 150..450 distance from a cosmetic clock.
    fn roll_walk(&self) -> Option<(bool, i32)> {
        let ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .subsec_nanos();
        if ns % 10 >= 3 {
            return None; // 30% chance (upstream walkChance)
        }
        Some((ns % 2 == 1, 150 + ((ns / 10) % 301) as i32))
    }

    pub fn tick(&mut self, now: u64) -> Option<&'static str> {
        // lazily stamp do-nothing-yet timers that were armed without a clock
        if let Some(Reaction::Charging { start }) = &mut self.reaction {
            if *start == LAZY_START {
                *start = now;
            }
        }
        if self.reset_phases {
            self.phase_index = 0;
            self.phase_anchor = Some(now);
            self.reset_phases = false;
        }
        if self.next_coffee == LAZY_START && matches!(self.mode, Mode::Action) {
            self.next_coffee = now + FIRST_COFFEE_AT;
        }

        // walk gate: upstream re-checks every 9s while the pet is awake
        if self.next_walk_check == LAZY_START {
            self.next_walk_check = now + WALK_CHECK_MS;
        } else if now >= self.next_walk_check {
            self.next_walk_check = now + WALK_CHECK_MS;
            if matches!(self.mode, Mode::Awake) && self.reaction.is_none() && self.walk.is_none() {
                self.walk_cycles += 1;
                if self.walk_cycles >= WALK_AFTER_CYCLES {
                    self.walk_cycles = 0;
                    if let Some((right, dist)) = self.roll_walk() {
                        self.begin_walk(now, right, dist);
                    }
                }
            }
        }

        // finished reactions fall back to the base pose on the same tick
        let expired = matches!(&self.reaction, Some(r) if r.frame(now).is_none());
        if expired {
            self.reaction = None;
            self.coffee_until = None;
        }

        if let Some(r) = &self.reaction {
            return r.frame(now);
        }

        if let Some(pose) = self.walk_frame(now) {
            return Some(pose);
        }

        match self.mode {
            Mode::Asleep => return Some("asleep"),
            Mode::Drowsy => {
                let anchor = *self.phase_anchor.get_or_insert(now);
                if now - anchor >= DROWSY_MS {
                    self.mode = Mode::Asleep;
                    return Some("asleep");
                }
                return Some("drowsy");
            }
            Mode::Action => {
                if self.coffee_until.is_none() && now >= self.next_coffee {
                    self.coffee_until = Some(now + COFFEE_LEN);
                    self.next_coffee = now + COFFEE_INTERVAL;
                }
                if self.coffee_until.is_some_and(|until| now < until) {
                    return Some("coffee-sip");
                }
                if self.spotify {
                    return Some("vibing");
                }
                // arm tap clock: the derived lifted frame plays the middle
                // half of each --type-speed cycle; taps tuned slower than
                // upstream's SVG rig (which moved two arms in 0.15s) so a
                // single bitmap arm reads as strokes, not flicker
                let speed = match self.cli_count {
                    0 | 1 => 250,
                    2 => 180,
                    _ => 120,
                };
                let quarter = (speed / 4).max(1);
                let phase = now % speed;
                return Some(if (quarter..quarter * 3).contains(&phase) {
                    "typing-press"
                } else {
                    "typing"
                });
            }
            Mode::Awake => {}
        }

        if self.spotify {
            return Some("vibing");
        }
        if self.battery_pct <= LOW_BATTERY_PCT && !self.charging {
            return Some("low-battery");
        }
        if self.hour >= 23 || self.hour < 5 {
            return Some("late-night");
        }
        self.advance_idle(now)
    }

    fn advance_idle(&mut self, now: u64) -> Option<&'static str> {
        let mut anchor = self.phase_anchor.unwrap_or(now);
        let mut idx = self.phase_index;
        // catch up over however much time passed since the last tick
        let mut guard = 0;
        while now >= anchor + IDLE_PHASES[idx].1 {
            anchor += IDLE_PHASES[idx].1;
            idx = (idx + 1) % IDLE_PHASES.len();
            guard += 1;
            if guard > IDLE_PHASES.len() + 2 {
                // clock ran far ahead (e.g. suspended); resync to now
                anchor = now;
                idx = 0;
                break;
            }
        }
        self.phase_anchor = Some(anchor);
        self.phase_index = idx;
        Some(IDLE_PHASES[idx].0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sensors::proc::Config;
    use crate::sensors::state::DerivedState;
    use serde_json::json;

    fn default_pet() -> Pet {
        Pet::new(&Config::parse(json!({})))
    }

    fn st(mode: &str, spotify: bool, charging: bool, pct: u32, hour: u32) -> DerivedState {
        DerivedState {
            mode: mode.to_string(),
            total_instances: 1,
            cli_count: 1,
            is_desktop_running: false,
            focused_process: String::new(),
            spotify_playing: spotify,
            hour,
            battery_pct: pct,
            is_charging: charging,
        }
    }

    #[cfg(test)]
    impl Pet {
        fn test_begin_walk(&mut self, right: bool, dist: i32, pos: i32, screen_w: i32, now: u64) {
            self.pos_x = pos;
            self.set_screen_size(screen_w as u32, 1000);
            self.begin_walk(now, right, dist);
        }
    }

    // ---- walk stride (derived swing twin) ----

    #[test]
    fn walking_strides_between_together_and_open_legs() {
        let mut p = default_pet();
        p.test_begin_walk(true, 300, 100, 1000, 0);
        // 0.4s stride cycle: base 0..200, swing 200..400
        assert_eq!(p.tick(0), Some("walk-right"));
        assert_eq!(p.tick(199), Some("walk-right"));
        assert_eq!(p.tick(200), Some("walk-right-swing"));
        assert_eq!(p.tick(399), Some("walk-right-swing"));
        assert_eq!(p.tick(400), Some("walk-right"));
        // left walk swings the mirrored twin
        let mut l = default_pet();
        l.test_begin_walk(false, 300, 100, 1000, 0);
        assert_eq!(l.tick(0), Some("walk-left"));
        assert_eq!(l.tick(250), Some("walk-left-swing"));
    }

    // ---- art bob (renderer.css walk-bob / vibe-bob ports) ----

    #[test]
    fn walking_bobs_and_settling_holds_still() {
        let mut p = default_pet();
        p.test_begin_walk(true, 300, 100, 1000, 0);
        // 0.4s walk-bob: half holds, other half hops a pixel
        assert_eq!(p.bounce_for("walk-right", 0), -1);
        assert_eq!(p.bounce_for("walk-right", 200), 0);
        assert_eq!(p.bounce_for("walk-left", 100), -1);
        // docked (Settling): the pet holds the ground
        assert_eq!(p.tick(3600), Some("walk-right"));
        assert_eq!(p.bounce_for("walk-right", 3601), 0);
        // non-animated poses are flat
        assert_eq!(p.bounce_for("typing", 5), 0);
        assert_eq!(p.bounce_for("idle", 5), 0);
    }

    #[test]
    fn vibing_bobs_on_its_own_060s_beat() {
        let mut p = default_pet();
        p.apply(&st("awake", true, false, 100, 12));
        assert_eq!(p.tick(0), Some("vibing"));
        assert_eq!(p.bounce_for("vibing", 299), -1);
        assert_eq!(p.bounce_for("vibing", 300), 0);
    }

    // ---- walk (upstream random-walk: 9s checks, 8 awake cycles, 30% chance) ----

    #[test]
    fn eyes_track_the_cursor_only_while_resting() {
        let mut p = default_pet();
        // cursor 200px to the right of the pet center (window coords)
        p.aim_eyes(400.0, 100.0);
        assert_eq!(p.eye_dx(), 1);
        // cursor far left
        p.aim_eyes(0.0, 100.0);
        assert_eq!(p.eye_dx(), -1);
        // dead centre
        p.aim_eyes(92.67, 116.5);
        assert_eq!(p.eye_dx(), 0);
        // eyes freeze outside a resting state
        p.apply(&st("action", false, false, 100, 12));
        assert_eq!(p.eye_dx(), 0);
    }


    #[test]
    fn walk_moves_right_then_docks_and_resumes_idle() {
        let mut p = default_pet();
        p.test_begin_walk(true, 300, 100, 1000, 0);
        assert_eq!(p.tick(0), Some("walk-right"));
        assert_eq!(p.tick(1600), Some("walk-right"));
        assert_eq!(p.pos().0, 250);
        // target reached at 3200ms: pose holds through the 200ms settle
        assert_eq!(p.tick(3400), Some("walk-right"));
        assert_eq!(p.pos().0, 400);
        // after settle the pet is back to its idle phase
        assert_eq!(p.tick(3600), Some("idle"));
        assert_eq!(p.pos().0, 400);
    }

    #[test]
    fn walk_target_clamps_at_the_left_wall() {
        let mut p = default_pet();
        p.test_begin_walk(false, 300, 100, 1000, 0);
        assert_eq!(p.tick(0), Some("walk-left"));
        // raw target 100-300 = -200 → clamped to 0, so the walk is shorter
        // (duration = 100px * 32/3 ms)
        assert_eq!(p.tick(500), Some("walk-left"));
        assert_eq!(p.tick(1300), Some("walk-left"));
        assert_eq!(p.pos().0, 0);
        assert_eq!(p.tick(1500), Some("idle"));
        assert_eq!(p.pos().0, 0);
    }

    #[test]
    fn walk_aborts_when_action_starts() {
        let mut p = default_pet();
        p.test_begin_walk(true, 300, 100, 1000, 0);
        assert_eq!(p.tick(0), Some("walk-right"));
        p.apply(&st("action", false, false, 100, 12));
        assert_eq!(p.tick(8250), Some("typing"));
        assert_eq!(p.pos().0, 100);
    }

    // ---- mode timeline ----

    #[test]
    fn awake_cycles_idle_phases_on_time() {
        let mut p = default_pet();
        p.apply(&st("awake", false, true, 100, 12));
        assert_eq!(p.tick(0), Some("idle"));
        assert_eq!(p.tick(3999), Some("idle"));
        assert_eq!(p.tick(4000), Some("look-right")); // phase switched
        assert_eq!(p.tick(5999), Some("look-right"));
        assert_eq!(p.tick(6000), Some("idle"));
        assert_eq!(p.tick(10_999), Some("look-left"), "9000+2000 window ends at 11000");
        assert_eq!(p.tick(11_000), Some("idle"));
    }

    #[test]
    fn asleep_goes_through_drowsy_with_upstream_4500ms() {
        let mut p = default_pet();
        p.apply(&st("awake", false, true, 100, 12));
        p.apply(&st("asleep", false, false, 100, 12)); // sensors say nothing is running
        assert_eq!(p.tick(10_000), Some("drowsy"));
        assert_eq!(p.tick(14_499), Some("drowsy"));
        assert_eq!(p.tick(14_500), Some("asleep"));
    }

    #[test]
    fn waking_cancels_drowsy_instantly() {
        let mut p = default_pet();
        p.apply(&st("asleep", false, false, 100, 12));
        let _ = p.tick(10_000);
        p.apply(&st("awake", false, false, 100, 12));
        assert_eq!(p.tick(10_100), Some("idle"));
    }

    // ---- action mode ----

    #[test]
    fn action_types_then_takes_coffee_break() {
        let mut p = default_pet();
        p.apply(&st("action", false, true, 100, 12));
        assert_eq!(p.tick(0), Some("typing"));
        assert!(matches!(p.tick(7_999), Some("typing" | "typing-press")));
        assert_eq!(p.tick(8_000), Some("coffee-sip")); // first break at 8s (upstream)
        assert_eq!(p.tick(14_999), Some("coffee-sip"));
        assert_eq!(p.tick(15_000), Some("typing"));
    }

    #[test]
    fn action_music_wins_over_typing() {
        let mut p = default_pet();
        p.apply(&st("action", true, true, 100, 12));
        assert_eq!(p.tick(0), Some("vibing"));
    }

    // ---- reactions ----

    #[test]
    fn click_reacts_happy_for_4s() {
        let mut p = default_pet();
        p.apply(&st("awake", false, true, 100, 12));
        p.on_click(100);
        assert_eq!(p.tick(150), Some("happy"));
        assert_eq!(p.tick(4_099), Some("happy"));
        assert_ne!(p.tick(4_100), Some("happy")); // idle resumes after
    }

    #[test]
    fn copy_sequence_full_timeline_from_clipboard_event() {
        let mut p = default_pet();
        p.apply(&st("awake", false, true, 100, 12));
        p.on_copy(1_000);
        assert_eq!(p.tick(1_100), Some("copy")); // grab phase
        assert_eq!(p.tick(1_500), Some("copy")); // hold phase (350+2000)
        assert_eq!(p.tick(2_600), Some("copy")); // done tail (350)
        assert_ne!(p.tick(3_701), Some("copy")); // idle again (window was [1000, 3700))
    }

    #[test]
    fn copy_survives_a_mode_switch_mid_reaction() {
        let mut p = default_pet();
        p.apply(&st("awake", false, true, 100, 12));
        p.on_copy(1_000);
        p.apply(&st("action", false, true, 100, 12)); // user jumped to Claude
        assert_eq!(p.tick(1_200), Some("copy"));
    }

    #[test]
    fn asleep_pet_ignores_clipboard_events() {
        let mut p = default_pet();
        p.apply(&st("asleep", false, false, 100, 12));
        let _ = p.tick(10_000);
        p.on_copy(10_100);
        assert_eq!(p.tick(10_200), Some("drowsy"), "upstream gate: reactions only while awake");
    }

    #[test]
    fn copy_never_fires_without_a_clipboard_event() {
        let mut p = default_pet();
        p.apply(&st("awake", false, true, 100, 12));
        for t in (0..60_000).step_by(100) {
            assert_ne!(p.tick(t), Some("copy"));
        }
    }

    #[test]
    fn screenshot_cheese_last_2_5s() {
        let mut p = default_pet();
        p.apply(&st("awake", false, true, 100, 12));
        p.on_screenshot(500);
        assert_eq!(p.tick(600), Some("cheese"));
        assert_eq!(p.tick(2_999), Some("cheese"));
        assert!(p.tick(3_001).is_some());
    }

    #[test]
    fn plug_in_triggers_charging_celebration() {
        let mut p = default_pet();
        p.apply(&st("awake", false, false, 100, 12));
        p.apply(&st("awake", false, true, 100, 12)); // charger appears
        assert_eq!(p.tick(3_200), Some("charging"));
        assert_eq!(p.tick(3_201).is_some(), true);
    }

    #[test]
    fn low_battery_frame_when_under_20_and_unplugged() {
        let mut p = default_pet();
        p.apply(&st("awake", false, false, 15, 12));
        assert_eq!(p.tick(0), Some("low-battery"));
    }

    #[test]
    fn late_night_uses_tired_frame() {
        let mut p = default_pet();
        p.apply(&st("awake", false, true, 100, 23));
        assert_eq!(p.tick(0), Some("late-night"));
        p.apply(&st("awake", false, true, 100, 4));
        assert_eq!(p.tick(100), Some("late-night"));
        p.apply(&st("awake", false, true, 100, 12));
        assert_ne!(p.tick(200), Some("late-night"));
    }
}

#[cfg(test)]
mod press_tests {
    use super::*;

    fn typing_pet(cli_count: u32) -> Pet {
        let mut p = Pet::new(&Config::parse(serde_json::json!({})));
        p.apply(&DerivedState {
            mode: "action".into(),
            total_instances: cli_count,
            cli_count,
            is_desktop_running: false,
            focused_process: "kitty".into(),
            spotify_playing: false,
            hour: 12,
            battery_pct: 90,
            is_charging: false,
        });
        p
    }

    #[test]
    fn action_flashes_the_lifted_frame_for_half_of_each_arm_cycle() {
        // 1 CLI: 250ms cycle → lifted in [62, 187)
        let mut p = typing_pet(1);
        assert_eq!(p.tick(0), Some("typing"));
        assert_eq!(p.tick(61), Some("typing"));
        assert_eq!(p.tick(62), Some("typing-press"));
        assert_eq!(p.tick(185), Some("typing-press"));
        assert_eq!(p.tick(186), Some("typing"));
        assert_eq!(p.tick(249), Some("typing"));
        assert_eq!(p.tick(250), Some("typing"));
        // 2 CLIs: 180ms → [45, 135); 3+: 120ms → [30, 90)
        let mut p2 = typing_pet(2);
        assert_eq!(p2.tick(44), Some("typing"));
        assert_eq!(p2.tick(45), Some("typing-press"));
        assert_eq!(p2.tick(134), Some("typing-press"));
        assert_eq!(p2.tick(135), Some("typing"));
        let mut p3 = typing_pet(3);
        assert_eq!(p3.tick(29), Some("typing"));
        assert_eq!(p3.tick(30), Some("typing-press"));
        assert_eq!(p3.tick(88), Some("typing-press"));
        assert_eq!(p3.tick(89), Some("typing-press"));
        assert_eq!(p3.tick(90), Some("typing"));
    }
}
