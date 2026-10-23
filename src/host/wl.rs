// Wayland host: the pet is a wlr-layer-shell OVERLAY surface — it floats
// above tiled windows, never takes focus, and never enters the taskbar.
// Position and drag live in the surface's anchored margins; pixels come
// from SHM buffers.
//
// shortcut: single-buffer present (commit while compositor may still read
// it; upgrade to double buffering if tearing shows).

use std::fs::File;
use std::io::{Seek, SeekFrom, Write};
use std::os::unix::io::AsFd;

use tempfile::tempfile;
use wayland_client::protocol::{
    wl_buffer, wl_compositor, wl_output, wl_pointer, wl_registry, wl_region, wl_seat, wl_shm,
    wl_shm_pool, wl_surface,
};
use wayland_client::{delegate_noop, Connection, Dispatch, EventQueue, QueueHandle, WEnum};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1, zwlr_layer_surface_v1,
};
use zwlr_layer_shell_v1::Layer;
use zwlr_layer_surface_v1::{Anchor, KeyboardInteractivity};

use crate::gfx::{Canvas, WINDOW_H, WINDOW_W};
use super::{input_regions, rgba_to_bgra, AppEvent, Host};

/// Where the pet perches by default (upstream margin config).
const PERCH_MARGIN: i32 = 24;

/// The dispatch state: owns the protocol proxies and reacts to events.
struct Inner {
    compositor: Option<wl_compositor::WlCompositor>,
    shm: Option<wl_shm::WlShm>,
    layer_shell: Option<zwlr_layer_shell_v1::ZwlrLayerShellV1>,
    seat: Option<wl_seat::WlSeat>,
    surface: Option<wl_surface::WlSurface>,
    layer_surface: Option<zwlr_layer_surface_v1::ZwlrLayerSurfaceV1>,
    buffer: Option<wl_buffer::WlBuffer>,
    pool_file: Option<File>,
    events: Vec<AppEvent>,
    passthrough: bool,
    last_canvas: Option<Canvas>,
    press_serial: u32,
    pointer_down: bool,
    pointer_moved: bool,
    pointer_prev: Option<(f64, f64)>,
    margin_right: i32,
    margin_bottom: i32,
    #[allow(dead_code)]
    pointer_holder: Option<wl_pointer::WlPointer>,
    #[allow(dead_code)]
    output_holder: Option<wl_output::WlOutput>,
    /// current output geometry from wl_output.mode (single-output assumption)
    screen: (i32, i32),
    configured: bool,
    qh: QueueHandle<Inner>,
}

pub struct WlHost {
    conn: Connection,
    queue: EventQueue<Inner>,
    inner: Inner,
}

impl WlHost {
    pub fn new() -> Result<WlHost, String> {
        let conn = Connection::connect_to_env().map_err(|e| format!("wayland connect: {e}"))?;
        let mut queue = conn.new_event_queue();
        let qh = queue.handle();

        let mut inner = Inner {
            compositor: None,
            shm: None,
            layer_shell: None,
            seat: None,
            surface: None,
            layer_surface: None,
            buffer: None,
            pool_file: None,
            events: Vec::new(),
            passthrough: false,
            last_canvas: None,
            press_serial: 0,
            pointer_down: false,
            pointer_moved: false,
            pointer_prev: None,
            margin_right: PERCH_MARGIN,
            margin_bottom: PERCH_MARGIN,
            pointer_holder: None,
            output_holder: None,
            screen: (0, 0),
            configured: false,
            qh: qh.clone(),
        };
        conn.display().get_registry(&qh, ());
        roundtrip(&mut queue, &mut inner, "globals")?;
        // the wl_output bind above fires a Mode event; catch it before use
        roundtrip(&mut queue, &mut inner, "output modes")?;

        let compositor = inner.compositor.clone().ok_or("no wl_compositor global")?;
        let shm = inner.shm.clone().ok_or("no wl_shm global")?;
        let layer_shell = inner
            .layer_shell
            .clone()
            .ok_or("no zwlr_layer_shell_v1 (pet needs an overlay-capable compositor)")?;
        if inner.seat.is_none() {
            eprintln!("clawd: no wl_seat global; clicks/drag disabled on this compositor");
        }

        let surface = compositor.create_surface(&qh, ());
        let layer_surface = layer_shell.get_layer_surface(
            &surface,
            None,
            Layer::Overlay,
            "clawd".to_owned(),
            &qh,
            (),
        );
        // bottom-right perch; -1 = overlap other layer surfaces, lets the
        // pet cross taskbars; None keyboard = never steals focus (chords
        // arrive via side channels instead)
        layer_surface.set_anchor(Anchor::Bottom | Anchor::Right);
        layer_surface.set_size(WINDOW_W, WINDOW_H);
        // send the perch immediately: without this the surface starts at
        // margin 0, glued to the screen corner (art reads as screen-edge-cut)
        layer_surface.set_margin(0, PERCH_MARGIN, PERCH_MARGIN, 0);
        layer_surface.set_exclusive_zone(-1);
        layer_surface.set_keyboard_interactivity(KeyboardInteractivity::None);

        // pool + fully transparent initial buffer (pet art goes in at present())
        let mut file = tempfile().map_err(|e| format!("tempfile: {e}"))?;
        file.set_len((WINDOW_W * WINDOW_H * 4) as u64)
            .map_err(|e| format!("set_len: {e}"))?;
        let size = (WINDOW_W * WINDOW_H * 4) as i32;
        let pool = shm.create_pool(file.as_fd(), size, &qh, ());
        let buffer = pool.create_buffer(
            0,
            WINDOW_W as i32,
            WINDOW_H as i32,
            (WINDOW_W * 4) as i32,
            wl_shm::Format::Argb8888,
            &qh,
            (),
        );
        inner.pool_file = Some(file);
        inner.buffer = Some(buffer);
        inner.surface = Some(surface);
        inner.layer_surface = Some(layer_surface);

        // layer-spec map dance: empty commit (no attach) requests the first
        // Configure; only then may a buffer be attached
        if let Some(s) = &inner.surface {
            s.commit();
        }
        roundtrip(&mut queue, &mut inner, "layer configure")?; // window is mapped

        Ok(WlHost { conn, queue, inner })
    }
}

fn roundtrip(queue: &mut EventQueue<Inner>, inner: &mut Inner, what: &str) -> Result<(), String> {
    queue
        .roundtrip(inner)
        .map(|_| ())
        .map_err(|e| format!("wayland {what}: {e}"))
}

fn commit_tree(inner: &Inner) {
    if let Some(s) = &inner.surface {
        if inner.configured {
            if let Some(buf) = &inner.buffer {
                s.attach(Some(buf), 0, 0);
                s.damage(0, 0, WINDOW_W as i32, WINDOW_H as i32);
            }
        }
        s.commit();
    }
}

fn apply_input_region(inner: &mut Inner) {
    let Some(surface) = &inner.surface else { return };
    let Some(compositor) = &inner.compositor else { return };
    let rects = match (inner.passthrough, &inner.last_canvas) {
        (true, _) => None,
        // idle but no frame yet: accept input everywhere until pixels show up
        (false, None) => None,
        (false, Some(c)) => Some(input_regions(c, false)),
    };
    match rects {
        None => {
            // empty input region: a region whose only rect was subtracted away
            let region = compositor.create_region(&inner.qh, ());
            region.add(0, 0, 1, 1);
            region.subtract(0, 0, 1, 1);
            surface.set_input_region(Some(&region));
        }
        Some(rs) => {
            let region = compositor.create_region(&inner.qh, ());
            for (x, y, w, h) in rs {
                region.add(x, y, w as i32, h as i32);
            }
            surface.set_input_region(Some(&region));
        }
    }
}

impl Host for WlHost {
    fn present(&mut self, c: &Canvas) {
        self.inner.last_canvas = Some(Canvas {
            width: c.width,
            height: c.height,
            buf: c.buf.clone(),
        });
        if let Some(f) = &mut self.inner.pool_file {
            let _ = f.seek(SeekFrom::Start(0));
            let _ = f.write_all(&rgba_to_bgra(&c.buf));
        }
        apply_input_region(&mut self.inner);
        commit_tree(&self.inner);
        // rs-buffered requests only leave on flush(); dispatch_pending does
        // NOT flush, and this host has no blocking dispatch in its loop
        let _ = self.conn.flush();
        let _ = self.queue.dispatch_pending(&mut self.inner);
    }

    fn size(&self) -> (u32, u32) {
        (WINDOW_W, WINDOW_H)
    }

    fn set_passthrough(&mut self, on: bool) {
        self.inner.passthrough = on;
        apply_input_region(&mut self.inner);
    }

    fn move_to(&mut self, x: i32, y: i32) -> Result<(), String> {
        // bottom-right anchor: a screen position maps back into margins
        let (sw, sh) = self.inner.screen;
        self.inner.margin_right = (sw - x - WINDOW_W as i32).max(0);
        self.inner.margin_bottom = (sh - y - WINDOW_H as i32).max(0);
        if let Some(ls) = &self.inner.layer_surface {
            ls.set_margin(0, self.inner.margin_right, self.inner.margin_bottom, 0);
        }
        commit_tree(&self.inner);
        let _ = self.conn.flush();
        Ok(())
    }

    fn position(&self) -> (i32, i32) {
        let (sw, sh) = self.inner.screen;
        if sw == 0 {
            return (0, 0);
        }
        (
            sw - self.inner.margin_right - WINDOW_W as i32,
            sh - self.inner.margin_bottom - WINDOW_H as i32,
        )
    }

    fn screen_size(&self) -> (u32, u32) {
        (self.inner.screen.0 as u32, self.inner.screen.1 as u32)
    }

    fn poll_events(&mut self) -> Vec<AppEvent> {
        // dispatch_pending only drains events already read off the socket;
        // pointer events are read here, non-blocking (nothing else ever
        // reads between frames)
        if let Some(guard) = self.conn.prepare_read() {
            let _ = guard.read();
        }
        let _ = self.queue.dispatch_pending(&mut self.inner);
        std::mem::take(&mut self.inner.events)
    }
}

#[cfg(test)]
mod live {
    use super::*;
    use crate::gfx::{blit_scale, Sheet, PAD, WINDOW_W};
    use std::path::PathBuf;
    use std::thread::sleep;
    use std::time::Duration;

    /// Peach (pet body) leftmost x on a screenshot; same art frame before and
    /// after the move makes the delta a clean position measure.
    fn peach_minx(path: &std::path::Path) -> i32 {
        let im = image::open(path).expect("screenshot decodes").to_rgb8();
        let (w, h) = (im.width() as i32, im.height() as i32);
        for x in 0..w {
            for y in 0..h {
                let p = im.get_pixel(x as u32, y as u32).0;
                if p == [222, 136, 109] {
                    return x;
                }
            }
        }
        panic!("no pet art found in {}", path.display());
    }

    fn latest_screenshot() -> std::path::PathBuf {
        let dir = std::path::PathBuf::from(
            std::env::var_os("HOME").expect("HOME"),
        ).join("Pictures/Screenshots");
        let mut best: Option<(std::time::SystemTime, std::path::PathBuf)> = None;
        for e in std::fs::read_dir(&dir).expect("screenshots dir") {
            let p = e.unwrap().path();
            if p.extension().and_then(|e| e.to_str()) != Some("png") { continue; }
            let m = std::fs::metadata(&p).and_then(|m| m.modified()).unwrap_or(
                std::time::SystemTime::UNIX_EPOCH);
            if best.as_ref().map(|(b, _)| m > *b).unwrap_or(true) {
                best = Some((m, p));
            }
        }
        best.expect("a screenshot exists").1
    }

    fn shot() -> std::path::PathBuf {
        std::process::Command::new("niri")
            .args(["msg", "action", "screenshot-screen"])
            .status().expect("niri");
        sleep(Duration::from_millis(800));
        // niri screenshots land under Pictures/Screenshots
        latest_screenshot()
    }

    #[test]
    #[ignore = "live: moves the pet 200px west and screenshots the delta (niri)"]
    fn walk_slides_the_pet_on_niri() {
        let media = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("media");
        let sheet = Sheet::load(&media);
        let frame = sheet.get("idle").expect("idle art").clone();
        let mut host = WlHost::new().expect("live compositor present");
        let mut canvas = crate::gfx::Canvas::new(WINDOW_W, crate::gfx::WINDOW_H);
        blit_scale(&mut canvas, &frame, PAD as i32, PAD as i32, 1, 0, 0);
        host.present(&canvas);
        sleep(Duration::from_secs(1));
        eprintln!("clawd live: screen={:?} pos={:?}", host.screen_size(), host.position());
        let a = peach_minx(&shot());
        let y = host.position().1;
        host.move_to(1500, y).expect("move");
        sleep(Duration::from_millis(600));
        let b = peach_minx(&shot());
        let dx = b - a;
        assert!((dx + 200).abs() <= 6, "pet must slide ~200px west, dx={dx}");
    }

    #[test]
    #[ignore = "live: overlays the pet on the actual desktop for ~5s (niri)"]
    fn overlays_idle_pose_on_niri() {
        let media = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("media");
        let sheet = Sheet::load(&media);
        let frame = sheet.get("idle").expect("idle art").clone();
        let mut host = WlHost::new().expect("live compositor present");
        let mut canvas = Canvas::new(WINDOW_W, WINDOW_H);
        blit_scale(&mut canvas, &frame, PAD as i32, PAD as i32, 1, 0, 0);
        host.set_passthrough(true);
        for i in 0..150 {
            host.present(&canvas);
            for ev in host.poll_events() {
                eprintln!("clawd live event: {ev:?}");
            }
            if i % 50 == 25 {
                host.set_passthrough(false);
            } else if i % 50 == 45 {
                host.set_passthrough(true);
            }
            sleep(Duration::from_millis(33));
        }
        // an overlay is NOT a window: niri must list the layer namespace but
        // never a tiled app
        if std::env::var_os("NIRI_SOCKET").is_some() {
            let layers = std::process::Command::new("niri")
                .args(["msg", "layers"])
                .output()
                .expect("niri present in PATH");
            assert!(
                String::from_utf8_lossy(&layers.stdout).contains("\"clawd\""),
                "pet layer surface must be listed by niri: {}",
                String::from_utf8_lossy(&layers.stdout)
            );
            let windows = std::process::Command::new("niri")
                .args(["msg", "--json", "windows"])
                .output()
                .unwrap();
            assert!(
                !String::from_utf8_lossy(&windows.stdout).contains("\"app_id\":\"clawd\""),
                "pet must not be a tiled window"
            );
        }
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for Inner {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global { name, interface, version } = event {
            match interface.as_str() {
                "wl_compositor" => {
                    state.compositor = Some(registry.bind(name, version.min(4), qh, ()));
                }
                "wl_shm" => state.shm = Some(registry.bind::<wl_shm::WlShm, _, _>(name, 1, qh, ())),
                "wl_seat" => {
                    state.seat = Some(registry.bind::<wl_seat::WlSeat, _, _>(name, 1, qh, ()))
                }
                "wl_output" => {
                    state.output_holder =
                        Some(registry.bind::<wl_output::WlOutput, _, _>(name, version.min(2), qh, ()));
                }
                "zwlr_layer_shell_v1" => {
                    state.layer_shell = Some(registry.bind::<
                        zwlr_layer_shell_v1::ZwlrLayerShellV1,
                        _,
                        _,
                    >(name, version.min(4), qh, ()))
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for Inner {
    fn event(
        state: &mut Self,
        seat: &wl_seat::WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities { capabilities: WEnum::Value(capabilities) } = event {
            if capabilities.contains(wl_seat::Capability::Pointer) {
                state.pointer_holder = Some(seat.get_pointer(qh, ()));
            }
        }
    }
}

impl Dispatch<wl_pointer::WlPointer, ()> for Inner {
    fn event(
        state: &mut Self,
        pointer: &wl_pointer::WlPointer,
        event: wl_pointer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use wl_pointer::Event as E;
        match event {
            E::Motion { surface_x, surface_y, .. } => {
                let (px, py) = state
                    .pointer_prev
                    .replace((surface_x, surface_y))
                    .unwrap_or((surface_x, surface_y));
                if state.pointer_down {
                    let dx = (surface_x - px) as i32;
                    let dy = (surface_y - py) as i32;
                    if dx != 0 || dy != 0 {
                        state.pointer_moved = true;
                        // drag = slide the anchored margins: moving the
                        // pointer right/down slides the pet the same way
                        // (its surface-local coords reset as the surface
                        // moves with it, so tracking does not drift)
                        state.margin_right = (state.margin_right - dx).max(0);
                        state.margin_bottom = (state.margin_bottom - dy).max(0);
                        if let Some(ls) = &state.layer_surface {
                            ls.set_margin(0, state.margin_right, state.margin_bottom, 0);
                        }
                        // commit lands with the next present tick (~33ms)
                    }
                } else {
                    // hover: eye-follow input (surface-local coords)
                    state.events.push(AppEvent::Cursor(surface_x, surface_y));
                }
            }
            E::Button { state: btn, serial, .. } => {
                match btn {
                    WEnum::Value(wl_pointer::ButtonState::Pressed) => {
                        state.pointer_down = true;
                        state.pointer_moved = false;
                        state.pointer_prev = None;
                        state.press_serial = serial;
                    }
                    WEnum::Value(wl_pointer::ButtonState::Released) => {
                        state.pointer_down = false;
                        let ev = if state.pointer_moved {
                            AppEvent::Dragged
                        } else {
                            AppEvent::Click
                        };
                        state.events.push(ev);
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_output::WlOutput, ()> for Inner {
    fn event(
        state: &mut Self,
        _: &wl_output::WlOutput,
        event: wl_output::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_output::Event::Mode { flags, width, height, .. } = event {
            if matches!(flags, WEnum::Value(f) if f.contains(wl_output::Mode::Current)) {
                state.screen = (width as i32, height as i32);
            }
        }
    }
}

impl Dispatch<zwlr_layer_surface_v1::ZwlrLayerSurfaceV1, ()> for Inner {
    fn event(
        state: &mut Self,
        ls: &zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwlr_layer_surface_v1::Event::Configure { serial, .. } = event {
            ls.ack_configure(serial);
            state.configured = true;
            commit_tree(state);
        }
    }
}

delegate_noop!(Inner: ignore wl_surface::WlSurface);
delegate_noop!(Inner: ignore wl_buffer::WlBuffer);
delegate_noop!(Inner: ignore wl_shm::WlShm);
delegate_noop!(Inner: ignore wl_shm_pool::WlShmPool);
delegate_noop!(Inner: ignore wl_region::WlRegion);
delegate_noop!(Inner: ignore wl_compositor::WlCompositor);
delegate_noop!(Inner: ignore zwlr_layer_shell_v1::ZwlrLayerShellV1);
