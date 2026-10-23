// Window hosts: present a composed pet frame in a borderless, transparent,
// always-on-top window. One module per display server behind the Host trait;
// both consume the same input-region math (clicks land on opaque pixels only).

#[cfg(feature = "host-wl")]
pub mod wl;
#[cfg(feature = "host-x11")]
pub(crate) mod x;

use crate::gfx::Canvas;

/// Pointer/window events the host extracts from the display server.
#[derive(Debug, PartialEq)]
pub enum AppEvent {
    /// pointer press-release without movement; the "pet me" gesture
    Click,
    /// press-release after movement (compositor drag); pet position changed
    Dragged,
    /// pointer hovering the pet (surface-local coords); eye-follow input
    Cursor(f64, f64),
    /// user wants clawd gone (heard via config/shortcut path; hosts may emit)
    Quit,
}

pub trait Host {
    fn present(&mut self, c: &Canvas);
    /// logical size of the pet window in px
    fn size(&self) -> (u32, u32);
    /// input region: None = passthrough, rects = opaque-art-only
    fn set_passthrough(&mut self, on: bool);
    /// move the pet window to a screen position (walking); bottom-anchored y
    fn move_to(&mut self, _x: i32, _y: i32) -> Result<(), String> {
        Ok(())
    }
    /// what move_to() considers the current window position
    fn position(&self) -> (i32, i32) {
        (0, 0)
    }
    /// screen size in px, for walk clamping (single output assumption)
    fn screen_size(&self) -> (u32, u32) {
        (0, 0)
    }
    /// drain pointer/window events accumulated since the last call
    fn poll_events(&mut self) -> Vec<AppEvent>;
}

/// Open a window host on whichever display server is running (Wayland first,
/// then X11 under XWayland). Call once from main; the app never switches.
pub fn open() -> Result<Box<dyn Host>, String> {
    if std::env::var_os("WAYLAND_DISPLAY").is_some() || std::env::var_os("WAYLAND_SOCKET").is_some()
    {
        #[cfg(feature = "host-wl")]
        return Ok(Box::new(
            wl::WlHost::new().map_err(|e| format!("wayland host: {e}"))?,
        ));
        #[cfg(not(feature = "host-wl"))]
        return Err("built without host-wl but WAYLAND_DISPLAY is set".into());
    }
    if std::env::var_os("DISPLAY").is_some() {
        #[cfg(feature = "host-x11")]
        return Ok(Box::new(
            x::XHost::new().map_err(|e| format!("x11 host: {e}"))?,
        ));
        #[cfg(not(feature = "host-x11"))]
        return Err("built without host-x11 but DISPLAY is set".into());
    }
    Err("no display server (WAYLAND_DISPLAY and DISPLAY both unset)".into())
}

/// XPutImage ZPixmap 32 and wl_shm ARGB8888 little-endian both want B,G,R,A.
pub(crate) fn rgba_to_bgra(src: &[u8]) -> Vec<u8> {
    src.chunks_exact(4)
        .flat_map(|p| [p[2], p[1], p[0], p[3]])
        .collect()
}

/// Opaque pixel rects (alpha > 32) used as the input region; empty means the
/// whole surface is click-through. Row runs are merged vertically when they
/// line up, so a blob pet becomes 1–3 rects, not hundreds of rows.
pub fn input_regions(c: &Canvas, idle: bool) -> Vec<(i32, i32, u32, u32)> {
    if idle {
        return Vec::new();
    }
    const ALPHA_MIN: u8 = 32;
    let mut rows: Vec<(i32, i32, u32)> = Vec::new(); // (y, x, len) runs per row
    for y in 0..c.height as i32 {
        let mut run_start: Option<i32> = None;
        for x in 0..c.width as i32 {
            let opaque =
                c.buf[((y as u32 * c.width + x as u32) * 4 + 3) as usize] > ALPHA_MIN;
            match (run_start, opaque) {
                (Some(s), true) => {}
                (Some(s), false) => {
                    rows.push((y, s, (x - s) as u32));
                    run_start = None;
                }
                (None, true) => run_start = Some(x),
                (None, false) => {}
            }
        }
        if let Some(s) = run_start {
            rows.push((y, s, (c.width as i32 - s) as u32));
        }
    }
    // vertical merge of runs that share (x, len) and touch y
    let mut rects: Vec<(i32, i32, u32, u32)> = Vec::new();
    for (y, x, len) in rows {
        let mut merged = false;
        for r in rects.iter_mut() {
            let (_, rx, rw, _) = *r;
            if r.0 + r.3 as i32 == y && rx == x && rw == len {
                r.3 += 1;
                merged = true;
                break;
            }
        }
        if !merged {
            rects.push((y, x, len, 1));
        }
    }
    // return as (x, y, w, h) for display-server order
    rects.iter().map(|(y, x, w, h)| (*x, *y, *w, *h)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canvas_with_opaque_bbox() -> Canvas {
        // 8x4; a 2x2 opaque block — blank frame border
        let mut c = Canvas::new(8, 4);
        for y in 1..3 {
            for x in 3..5 {
                let i = ((y as u32 * 8 + x as u32) * 4) as usize;
                c.buf[i] = 10;
                c.buf[i + 3] = 255;
            }
        }
        c
    }

    #[test]
    fn idle_pet_is_clickthrough_everywhere() {
        let c = canvas_with_opaque_bbox();
        assert!(input_regions(&c, true).is_empty(), "resting pet must not eat clicks");
    }

    #[test]
    fn active_pet_becomes_clickable_in_one_rect() {
        let c = canvas_with_opaque_bbox();
        let regions = input_regions(&c, false);
        assert_eq!(regions.len(), 1, "contiguous opaque pixels merge into one rect");
        let (x, y, w, h) = regions[0];
        assert_eq!((x, y, w, h), (3, 1, 2, 2));
    }

    #[test]
    fn fully_transparent_canvas_accepts_no_input() {
        let c = Canvas::new(8, 4);
        assert!(input_regions(&c, false).is_empty());
    }
}
