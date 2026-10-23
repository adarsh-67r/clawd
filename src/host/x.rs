// X11 host: ARGB32 pet window, XShape input region, XMoveWindow.
// Keyboard chords + eye-follow land here later (Task 9); the host only
// surfaces pointer events to the pose engine.

#[cfg(test)]
mod tests {
    use super::*;
    use x11rb::protocol::xproto::{VisualClass, Visualtype};

    #[test]
    fn picks_the_32bit_truecolour_visual() {
        let visual = Visualtype {
            visual_id: 9,
            class: VisualClass::TRUE_COLOR,
            bits_per_rgb_value: 8,
            colormap_entries: 256,
            red_mask: 0x00ff0000,
            green_mask: 0x0000ff00,
            blue_mask: 0x000000ff,
        };
        let non_argb = Visualtype {
            visual_id: 3,
            class: VisualClass::TRUE_COLOR,
            bits_per_rgb_value: 8,
            colormap_entries: 256,
            red_mask: 0xf800,
            green_mask: 0x7e0,
            blue_mask: 0x1f,
        };
        let list = [non_argb, visual];
        let picked = pick_argb_visual(&list);
        assert_eq!(
            picked.map(|v| v.visual_id),
            Some(9),
            "must pick only the ARGB32 TrueColor visual"
        );
    }

    #[test]
    fn rgba_becomes_bgra_for_zpixmap() {
        let rgba = [10u8, 20, 30, 255, 1, 2, 3, 128];
        let bgra = rgba_to_bgra(&rgba);
        assert_eq!(bgra, vec![30, 20, 10, 255, 3, 2, 1, 128]);
    }
}

// ---- implementation ----

use x11rb::connection::Connection;
use x11rb::protocol::shape::{ConnectionExt as _, SK, SO};
use x11rb::protocol::xproto::*;
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;

use super::{input_regions, rgba_to_bgra, AppEvent, Host};
use crate::gfx::{Canvas, FRAME_H, FRAME_W, PAD, WINDOW_H, WINDOW_W};

/// The ARGB32 visual is the TrueColor one whose masks span full bytes.
fn pick_argb_visual(visuals: &[Visualtype]) -> Option<&Visualtype> {
    visuals.iter().find(|v| {
        v.class == VisualClass::TRUE_COLOR
            && v.red_mask == 0x00ff_0000
            && v.green_mask == 0x0000_ff00
            && v.blue_mask == 0x0000_00ff
    })
}

pub struct XHost {
    conn: RustConnection,
    win: Window,
    root: Window,
    depth: u8,
    gc: Gcontext,
    passthrough: bool,
    last_canvas: Option<Canvas>,
    pointer_down: bool,
    moved: bool,
    last_pos: (i16, i16),
    win_pos: (i32, i32),
    screen_width_in_pixels: u16,
    screen_height_in_pixels: u16,
    pending: Vec<AppEvent>,
}

impl XHost {
    pub fn new() -> Result<XHost, String> {
        let (conn, screen_idx) = RustConnection::connect(None)
            .map_err(|e| format!("x11 connect: {e}"))?;
        let screen = conn.setup().roots[screen_idx].clone();

        let mut argb = None;
        for d in &screen.allowed_depths {
            if d.depth == 32 {
                if let Some(v) = pick_argb_visual(&d.visuals) {
                    argb = Some((v.visual_id, d.depth));
                    break;
                }
            }
        }
        let (visual_id, depth) =
            argb.ok_or("x11: no 32-bit ARGB visual (composite manager required)")?;

        let win = conn.generate_id().map_err(|e| e.to_string())?;
        let cmap = conn.generate_id().map_err(|e| e.to_string())?;
        let gc = conn.generate_id().map_err(|e| e.to_string())?;

        conn.create_colormap(ColormapAlloc::NONE, cmap, screen.root, visual_id)
            .map_err(|e| e.to_string())?;
        conn.create_window(
            depth,
            win,
            screen.root,
            0,
            0,
            WINDOW_W as u16,
            WINDOW_H as u16,
            0,
            WindowClass::INPUT_OUTPUT,
            visual_id,
            &CreateWindowAux::new()
                .colormap(cmap)
                .border_pixel(screen.black_pixel)
                .event_mask(
                    EventMask::BUTTON_PRESS | EventMask::BUTTON_RELEASE | EventMask::BUTTON_MOTION,
                ),
        )
        .map_err(|e| e.to_string())?;
        conn.create_gc(gc, win, &CreateGCAux::new())
            .map_err(|e| e.to_string())?;

        set_above(&conn, screen.root, win)?;
        conn.map_window(win).map_err(|e| e.to_string())?;
        conn.flush().map_err(|e| e.to_string())?;

        Ok(XHost {
            conn,
            win,
            root: screen.root,
            depth,
            gc,
            passthrough: false,
            last_canvas: None,
            pointer_down: false,
            moved: false,
            last_pos: (0, 0),
            win_pos: (0, 0),
            screen_width_in_pixels: screen.width_in_pixels,
            screen_height_in_pixels: screen.height_in_pixels,
            pending: Vec::new(),
        })
    }

    fn apply_input_region(&self) {
        let rects: Vec<Rectangle> = match (self.passthrough, &self.last_canvas) {
            (true, _) => Vec::new(),
            // no frame yet: whole window takes input until pixels land
            (false, None) => vec![Rectangle {
                x: 0,
                y: 0,
                width: WINDOW_W as u16,
                height: WINDOW_H as u16,
            }],
            (false, Some(c)) => input_regions(c, false)
                .into_iter()
                .map(|(x, y, w, h)| Rectangle {
                    x: x as i16,
                    y: y as i16,
                    width: w as u16,
                    height: h as u16,
                })
                .collect(),
        };
        let _ = self.conn.shape_rectangles(
            SO::SET,
            SK::INPUT,
            ClipOrdering::UNSORTED,
            self.win,
            0,
            0,
            &rects,
        );
        let _ = self.conn.flush();
    }

    /// Window position in root coordinates (the WM picks the initial spot).
    fn anchor(&self) -> Option<(i32, i32)> {
        let r = self
            .conn
            .translate_coordinates(self.win, self.root, 0, 0)
            .ok()?
            .reply()
            .ok()?;
        Some((r.dst_x as i32, r.dst_y as i32))
    }
}

fn set_above(conn: &RustConnection, root: Window, win: Window) -> Result<(), String> {
    let atom = |name: &[u8]| -> u32 {
        conn.intern_atom(false, name)
            .ok()
            .and_then(|c| c.reply().ok())
            .map(|r| r.atom)
            .unwrap_or(0)
    };
    let state = atom(b"_NET_WM_STATE");
    let above = atom(b"_NET_WM_STATE_ABOVE");
    let skip_taskbar = atom(b"_NET_WM_STATE_SKIP_TASKBAR");
        let data = ClientMessageData::from([1u32, above, skip_taskbar, 0, 0]);
        let ev = ClientMessageEvent::new(32, win, state, data);
    conn.send_event(
        false,
        root,
        EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
        ev,
    )
    .map_err(|e| format!("x11 _NET_WM_STATE: {e}"))?;
    Ok(())
}

impl Host for XHost {
    fn size(&self) -> (u32, u32) {
        (WINDOW_W, WINDOW_H)
    }

    fn present(&mut self, c: &Canvas) {
        self.last_canvas = Some(Canvas {
            width: c.width,
            height: c.height,
            buf: c.buf.clone(),
        });
        let data = rgba_to_bgra(&c.buf);
        let _ = self.conn.put_image(
            ImageFormat::Z_PIXMAP,
            self.win,
            self.gc,
            c.width as u16,
            c.height as u16,
            0,
            0,
            0,
            self.depth,
            &data,
        );
        self.apply_input_region();
        let _ = self.conn.flush();
    }

    fn set_passthrough(&mut self, on: bool) {
        self.passthrough = on;
        self.apply_input_region();
    }

    fn move_to(&mut self, x: i32, y: i32) -> Result<(), String> {
        let (wx, wy) = self.anchor().unwrap_or((x, y));
        if wx != x || wy != y {
            let _ = self
                .conn
                .configure_window(self.win, &ConfigureWindowAux::new().x(x).y(y));
        }
        self.win_pos = (x, y);
        let _ = self.conn.flush();
        Ok(())
    }

    fn position(&self) -> (i32, i32) {
        self.anchor().unwrap_or(self.win_pos)
    }

    fn screen_size(&self) -> (u32, u32) {
        (self.screen_width_in_pixels as u32, self.screen_height_in_pixels as u32)
    }

    fn poll_events(&mut self) -> Vec<AppEvent> {
        while let Ok(Some(ev)) = self.conn.poll_for_event() {
            match ev {
                Event::ButtonPress(e) => {
                    self.pointer_down = true;
                    self.moved = false;
                    self.last_pos = (e.root_x, e.root_y);
                    if let Some(pos) = self.anchor() {
                        self.win_pos = pos;
                    }
                }
                Event::MotionNotify(e) if self.pointer_down => {
                    let dx = e.root_x as i32 - self.last_pos.0 as i32;
                    let dy = e.root_y as i32 - self.last_pos.1 as i32;
                    if dx != 0 || dy != 0 {
                        self.moved = true;
                        let (x, y) = (self.win_pos.0 + dx, self.win_pos.1 + dy);
                        let _ = self
                            .conn
                            .configure_window(self.win, &ConfigureWindowAux::new().x(x).y(y));
                        self.win_pos = (x, y);
                        self.last_pos = (e.root_x, e.root_y);
                    }
                }
                Event::ButtonRelease(_) => {
                    self.pointer_down = false;
                    self.pending
                        .push(if self.moved { AppEvent::Dragged } else { AppEvent::Click });
                }
                _ => {}
            }
        }
        std::mem::take(&mut self.pending)
    }
}

#[cfg(test)]
mod live {
    use super::*;
    use crate::gfx::{blit_scale, Sheet};
    use std::path::PathBuf;
    use std::thread::sleep;
    use std::time::Duration;

    #[test]
    #[ignore = "live X11: draws the pet on a real X server for ~3s"]
    fn presents_idle_pose_on_x11() {
        let media = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("media");
        let sheet = Sheet::load(&media);
        let frame = sheet.get("idle").expect("idle art");
        let mut host = XHost::new().expect("X server present");
        let mut canvas = Canvas::new(WINDOW_W, WINDOW_H);
        blit_scale(&mut canvas, frame, PAD as i32, PAD as i32, 1, 0, 0);
       	host.set_passthrough(true);
        for i in 0..90 {
            host.present(&canvas);
            for ev in host.poll_events() {
                eprintln!("clawd x11 live event: {ev:?}");
            }
            if i == 40 {
                host.set_passthrough(false);
            } else if i == 80 {
                host.set_passthrough(true);
            }
            sleep(Duration::from_millis(33));
        }
    }
}
