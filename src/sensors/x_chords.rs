// X11-only keyboard chords (ctrl+c / ctrl+v / Print / super+shift+s).
// Reads the X server's global keymap bitmask (XQueryKeymap); no grabs, no
// helper processes. Wayland sessions never construct this — the clipboard
// watcher carries the copy signal there instead.

use x11rb::connection::Connection;
use x11rb::protocol::xproto::ConnectionExt;
use x11rb::rust_connection::RustConnection;

use super::linux::KeyCommand;

// keysyms from the X11 keysymdef list
const CTRL: &[u32] = &[65507, 65508]; // Control_L, Control_R
const SHIFT: &[u32] = &[65505, 65506]; // Shift_L, Shift_R
const SUPER: &[u32] = &[65515, 65516]; // Super_L, Super_R
const PRINT: &[u32] = &[65377]; // Print

const KEY_C: u32 = 0x63;
const KEY_V: u32 = 0x76;
const KEY_S: u32 = 0x73;

/// Pure chord decision from the set of pressed keysyms (unit-tested without X).
pub fn chord(pressed: &[u32]) -> Option<KeyCommand> {
    let ctrl = |syms: &[u32]| syms.iter().any(|k| pressed.contains(k));
    if ctrl(CTRL) {
        if pressed.contains(&KEY_C) {
            return Some(KeyCommand::Copy);
        }
        if pressed.contains(&KEY_V) {
            return Some(KeyCommand::Paste);
        }
    }
    if ctrl(PRINT) || (ctrl(SUPER) && ctrl(SHIFT) && pressed.contains(&KEY_S)) {
        return Some(KeyCommand::Screenshot);
    }
    None
}

pub struct ChordSensor {
    conn: RustConnection,
    /// keysym table, row-major over keycodes min_keycode..=max_keycode
    syms: Vec<u32>,
    per: usize,
    last_copy: std::time::Instant,
    last_paste: std::time::Instant,
    last_shot: std::time::Instant,
}

impl ChordSensor {
    pub fn new() -> Result<Self, String> {
        let (conn, _) = RustConnection::connect(None).map_err(|e| format!("x11 connect: {e}"))?;
        let setup = conn.setup();
        let count = setup.max_keycode.wrapping_sub(setup.min_keycode).wrapping_add(1);
        let rep = conn
            .get_keyboard_mapping(setup.min_keycode, count)
            .map_err(|e| format!("keyboard mapping: {e}"))?
            .reply()
            .map_err(|e| format!("keyboard mapping: {e}"))?;
        let per = rep.keysyms_per_keycode.max(1) as usize;
        let syms: Vec<u32> = rep.keysyms.iter().map(|&s| s as u32).collect();
        Ok(Self {
            conn,
            syms,
            per,
            last_copy: far_past(),
            last_paste: far_past(),
            last_shot: far_past(),
        })
    }

    /// keysyms currently held down, from the 32-byte pressed-bitmask
    fn pressed(&self) -> Vec<u32> {
        let Ok(rep) = self.conn.query_keymap() else {
            return Vec::new();
        };
        let Ok(rep) = rep.reply() else { return Vec::new() };
        let min = self.conn.setup().min_keycode as u16;
        let mut out = Vec::new();
        for (byte, &b) in rep.keys.iter().enumerate() {
            for bit in 0..8 {
                if b & (1 << bit) == 0 {
                    continue;
                }
                let keycode = min + (bytes_8(byte, bit) as u16);
                let idx = keycode.wrapping_sub(min) as usize * self.per;
                if idx + self.per <= self.syms.len() {
                    out.extend(self.syms[idx..idx + self.per].iter().copied().filter(|&s| s != 0));
                }
            }
        }
        out
    }

    pub fn poll(&mut self) -> Option<KeyCommand> {
        let cmd = chord(&self.pressed())?;
        let now = std::time::Instant::now();
        let (slot, gap) = match cmd {
            KeyCommand::Copy => (&mut self.last_copy, 500),
            KeyCommand::Paste => (&mut self.last_paste, 500),
            KeyCommand::Screenshot => (&mut self.last_shot, 1000),
        };
        if now.duration_since(*slot).as_millis() <= gap {
            return None;
        }
        *slot = now;
        Some(cmd)
    }
}

fn bytes_8(byte: usize, bit: u32) -> u8 {
    (byte * 8 + bit as usize) as u8
}

fn far_past() -> std::time::Instant {
    std::time::Instant::now() - std::time::Duration::from_secs(10)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chords_decide_from_pressed_keysyms() {
        assert_eq!(chord(&[65507, KEY_C]), Some(KeyCommand::Copy));
        assert_eq!(chord(&[65508, KEY_C]), Some(KeyCommand::Copy));
        assert_eq!(chord(&[65507, KEY_V]), Some(KeyCommand::Paste));
        // Print alone and super+shift+s both screenshot
        assert_eq!(chord(&[65377]), Some(KeyCommand::Screenshot));
        assert_eq!(chord(&[65515, 65505, KEY_S]), Some(KeyCommand::Screenshot));
        // no false positives
        assert_eq!(chord(&[65505, KEY_S]), None, "shift+s without super");
        assert_eq!(chord(&[65507]), None, "ctrl alone");
        assert_eq!(chord(&[KEY_C]), None, "c without ctrl");
        // ctrl+s is not a chord
        assert_eq!(chord(&[65507, KEY_S]), None);
    }
}
