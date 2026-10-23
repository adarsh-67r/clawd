// Pixel-art asset sheet and frame compositor.
//
// Poses are full-frame PNGs in media/ (190x200, 8px pixel grid); pose names
// are the file stems. Frame composition is: integer-scale blit +
// eye-translate + vertical bounce, all nearest-neighbour.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub const FRAME_W: u32 = 190;
pub const FRAME_H: u32 = 200;

#[derive(Clone)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    rgba: Vec<u8>,
}

impl Frame {
    pub fn pixels(&self) -> &[u8] {
        &self.rgba
    }

    /// Alpha at pixel (x, y); 0 outside the frame.
    pub fn alpha_at(&self, x: i32, y: i32) -> u8 {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return 0;
        }
        self.rgba[((y as u32 * self.width + x as u32) * 4 + 3) as usize]
    }
}

pub struct Canvas {
    pub width: u32,
    pub height: u32,
    pub buf: Vec<u8>,
}

impl Canvas {
    pub fn new(width: u32, height: u32) -> Canvas {
        Canvas {
            width,
            height,
            buf: vec![0; (width * height * 4) as usize],
        }
    }

    #[cfg(test)]
    pub fn alpha_at(&self, x: i32, y: i32) -> u8 {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return 0;
        }
        self.buf[((y as u32 * self.width + x as u32) * 4 + 3) as usize]
    }
}

/// All pose frames loaded once at startup; pose id = media file stem.
pub struct Sheet {
    frames: HashMap<String, Frame>,
}

/// The typing art's arm blob (dark-orange shade), located in media/typing.png.
pub const ARM_COLOR: [u8; 3] = [201, 119, 93];

/// The window carries 4px of transparent gutter on every side so art that
/// reaches the frame edge (the coffee mug) doesn't sit against the window
/// border and read as cut off.
pub const PAD: u32 = 4;
pub const WINDOW_W: u32 = FRAME_W + 2 * PAD;
pub const WINDOW_H: u32 = FRAME_H + 2 * PAD;

/// Derive the walk stride twin from a walk frame: leg pixels shear outward
/// from the torso centre (0px at the hip, 3px at the foot), giving the
/// open-stride pose that alternates with the base (legs together). The
/// black ground shadow stays put. Bitmap stand-in for the SVG rig's
/// rotating leg pivots.
fn derive_walk_swing(base: &Frame) -> Frame {
    let w = base.width as usize;
    let h = base.height as usize;
    let mut rgba = base.pixels().to_vec();
    // leg band: the 8 pixel rows above the ground shadow
    const LEG_TOP: i32 = 162;
    const LEG_BOT: i32 = 169;
    // leg pixels (opaque, not shadow-black)
    let leg = |x: usize, y: usize| {
        if (y as i32) < LEG_TOP || (y as i32) > LEG_BOT {
            return false;
        }
        let i = (y * w + x) * 4;
        rgba[i] != 0 && rgba[i + 3] > 0
    };
    // torso centre = midpoint of the leg band's extent
    let xs: Vec<usize> = (0..w)
        .filter(|&x| (0..=8).any(|k| leg(x, LEG_TOP as usize + k)))
        .collect();
    let Some(&lo) = xs.first() else { return base.clone() };
    let Some(&hi) = xs.last() else { return base.clone() };
    let mid = (hi + lo) as f64 / 2.0;
    // shear ramp: 0px at the hip row, 5px at the foot row (upstream pivots
    // the legs ±25°; 5px at 8 rows ≈ the same foot travel, legibly)
    let span = (LEG_BOT - LEG_TOP) as f64;
    let shear = |y: usize| -> f64 { (y as f64 - LEG_TOP as f64) * 5.0 / span };
    // collect sources before writing (dests may overlap sources within a row)
    let mut moves: Vec<(usize, usize, [u8; 4])> = Vec::new();
    for y in LEG_TOP as usize..=LEG_BOT as usize {
        for x in 0..w {
            if !leg(x, y) {
                continue;
            }
            let dx = if (x as f64) < mid { -shear(y) } else { shear(y) };
            let nx = (x as f64 + dx).round() as i64;
            if nx < 0 || nx >= w as i64 {
                continue;
            }
            let src = y * w + x;
            let mut px = [0u8; 4];
            px.copy_from_slice(&rgba[src * 4..src * 4 + 4]);
            moves.push((y * w + nx as usize, src, px));
        }
    }
    // pass 1: clear every source cell; pass 2: write the shifted pixels
    for (_, src, _) in &moves {
        rgba[src * 4..src * 4 + 4].fill(0);
    }
    for (d, _, px) in moves {
        rgba[d * 4..d * 4 + 4].copy_from_slice(&px);
    }
    Frame { width: base.width, height: base.height, rgba }
}

/// Derive the second typing frame from `typing`: the arm blob lifts 2px so
/// the stroke reads at presentation scale (upstream lifts 1px on the SVG
/// rig; the bitmap needs a deeper bob to stop reading as flicker); the
/// rows it vacates show what was under the arm (read before the mutation).
/// Only pixels of exactly ARM_COLOR move, so the body stays still.
fn derive_typing_press(base: &Frame) -> Frame {
    let w = base.width as usize;
    let h = base.height as usize;
    let mut rgba = base.pixels().to_vec();
    let is_arm = |y: usize, x: usize| {
        let i = (y * w + x) * 4;
        rgba[i..i + 3] == ARM_COLOR && rgba[i + 3] > 0
    };
    // mask from the unmutated base so shifted pixels can't re-trigger
    let mut lifted = vec![false; w * h];
    for y in 1..h {
        for x in 0..w {
            if is_arm(y, x) {
                lifted[y * w + x] = true;
            }
        }
    }
    // what each column's vacated bottom cells will show (the content that
    // was under the arm), captured before any pixel moves
    let mut fills: Vec<(usize, [u8; 4])> = Vec::new();
    for x in 0..w {
        for y in 1..h {
            if lifted[y * w + x] && (y + 2 >= h || !lifted[(y + 2) * w + x]) {
                let mut px = [0u8; 4];
                if y + 2 < h {
                    let s = ((y + 2) * w + x) * 4;
                    px.copy_from_slice(&rgba[s..s + 4]);
                }
                fills.push((y * w + x, px));
            }
        }
    }
    // lift: every arm pixel moves up 2, sources read before being overwritten
    // (destinations sit strictly above sources; blob rows start below y=2)
    for y in 2..h {
        for x in 0..w {
            if lifted[y * w + x] {
                let (s, d) = ((y * w + x) * 4, ((y - 2) * w + x) * 4);
                let mut px = [0u8; 4];
                px.copy_from_slice(&rgba[s..s + 4]);
                rgba[d..d + 4].copy_from_slice(&px);
            }
        }
    }
    // today only whole-frame cells vacate: clear + restore the underlying art
    for (i, px) in fills {
        if px[3] > 0 {
            rgba[i * 4..i * 4 + 4].copy_from_slice(&px);
        }
    }
    Frame {
        width: base.width,
        height: base.height,
        rgba,
    }
}

impl Sheet {
    pub fn load(media_dir: &Path) -> Sheet {
        let mut frames = HashMap::new();
        if let Ok(read) = std::fs::read_dir(media_dir) {
            let mut paths: Vec<PathBuf> = read
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|e| e == "png"))
                .collect();
            paths.sort();
            for p in paths {
                let name = p
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or_default()
                    .to_owned();
                if let Ok(frame) = decode_png(&p) {
                    frames.insert(name, frame);
                }
            }
        }
        // The typing pose animates by alternating two frames; only one PNG
        // exists, so the pressed-down twin is derived here at load time.
        // Same for the walk bitmap: the open-stride twin is derived so the
        // legs actually animate while the pet moves.
        if let Some(typing) = frames.get("typing").cloned() {
            frames.insert("typing-press".to_owned(), derive_typing_press(&typing));
        }
        for dir in ["walk-right", "walk-left"] {
            if let Some(walk) = frames.get(dir).cloned() {
                frames.insert(format!("{dir}-swing"), derive_walk_swing(&walk));
            }
        }
        Sheet { frames }
    }

    pub fn get(&self, pose: &str) -> Option<&Frame> {
        self.frames.get(pose)
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.frames.len()
    }
}

fn decode_png(path: &Path) -> image::ImageResult<Frame> {
    let img = image::ImageReader::open(path)?.decode()?;
    let rgba = img.to_rgba8();
    let (width, height) = (rgba.width(), rgba.height());
    Ok(Frame {
        width,
        height,
        rgba: rgba.into_raw(),
    })
}

/// Draw `src` onto `dst` at integer `scale`, offset (dx, dy), with extra
/// pixel offsets for the eye transform and vertical bounce. Nearest
/// neighbour, straight alpha over (destination keeps existing content).
pub fn blit_scale(
    dst: &mut Canvas,
    src: &Frame,
    dx: i32,
    dy: i32,
    scale: u32,
    eye_dx: i32,
    bounce: i32,
) {
    let dx = dx + eye_dx;
    let dy = dy + bounce;
    for sy in 0..src.height {
        for sx in 0..src.width {
            let px = src.rgba[((sy * src.width + sx) * 4) as usize];
            let py = src.rgba[((sy * src.width + sx) * 4 + 1) as usize];
            let pb = src.rgba[((sy * src.width + sx) * 4 + 2) as usize];
            let a = src.rgba[((sy * src.width + sx) * 4 + 3) as usize];
            if a == 0 && scale > 1 {
                continue;
            }
            for oy in 0..scale {
                let dy2 = dy + (sy as u32 * scale + oy) as i32;
                if dy2 < 0 || dy2 >= dst.height as i32 {
                    continue;
                }
                for ox in 0..scale {
                    let dx2 = dx + (sx as u32 * scale + ox) as i32;
                    if dx2 < 0 || dx2 >= dst.width as i32 {
                        continue;
                    }
                    let di = ((dy2 as u32 * dst.width + dx2 as u32) * 4) as usize;
                    if a == 255 {
                        dst.buf[di] = px;
                        dst.buf[di + 1] = py;
                        dst.buf[di + 2] = pb;
                        dst.buf[di + 3] = 255;
                    } else {
                        // straight alpha over
                        let da = dst.buf[di + 3] as u32;
                        let oa = a as u32;
                        let out = 255 - (255 - oa) * (255 - da) / 255;
                        if out > 0 {
                            for c in 0..3 {
                                let sc = [px, py, pb][c] as u32;
                                let dc = dst.buf[di + c] as u32;
                                let v = (sc * oa + dc * da * (255 - oa) / 255) / out.max(1);
                                dst.buf[di + c] = v.min(255) as u8;
                            }
                            dst.buf[di + 3] = out as u8;
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn media() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("media")
    }

    #[test]
    fn idle_decodes_and_is_pixelart() {
        let sheet = Sheet::load(&media());
        let f = sheet.get("idle").expect("idle pose exists");
        assert_eq!((f.width, f.height), (FRAME_W, FRAME_H));
        // centre pixel is the pet body: fully opaque
        let cx = (f.width / 2) as i32;
        let cy = (f.height / 2) as i32;
        assert_eq!(f.alpha_at(cx, cy), 255);
        // corner is transparent (canvas has no background)
        assert_eq!(f.alpha_at(0, 0), 0);
    }

    #[test]
    fn scaling_is_nearest_neighbour() {
        let mut src = Frame {
            width: 2,
            height: 2,
            rgba: vec![255, 0, 0, 255, 0, 0, 0, 0, 0, 0, 0, 0, 0, 255, 0, 255],
        };
        let mut dst = Canvas::new(4, 4);
        blit_scale(&mut dst, &mut src, 0, 0, 2, 0, 0);
        // a 1-px opaque source covers exactly an N×N block, no interpolation
        assert_eq!(dst.alpha_at(0, 0), 255);
        assert_eq!(dst.alpha_at(1, 1), 255);
        assert_eq!(dst.alpha_at(2, 0), 0);
        assert_eq!(dst.alpha_at(0, 2), 0);
        // the blended pixel keeps its own hue (no mix with red)
        assert_eq!(dst.buf[((2 * 4 + 2) * 4 + 1) as usize], 255);
    }
}

#[cfg(test)]
mod walk_swing_tests {
    use super::*;

    #[test]
    fn swing_spreads_the_legs_and_keeps_the_torso() {
        let media = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("media");
        let sheet = Sheet::load(&media);
        let base = sheet.get("walk-right").unwrap();
        let swing = sheet.get("walk-right-swing").expect("derived walk swing frame");
        assert_eq!((swing.width, swing.height), (FRAME_W, FRAME_H));
        let leg_lo = |f: &Frame| {
            (0..f.width as usize)
                .find(|&x| (162..=169).any(|y| f.alpha_at(x as i32, y as i32) > 0))
                .unwrap()
        };
        let leg_hi = |f: &Frame| {
            let w = f.width as usize;
            (0..w)
                .rev()
                .find(|&x| (162..=169).any(|y| f.alpha_at(x as i32, y as i32) > 0))
                .unwrap()
        };
        // legs shear outward on both sides
        assert!(leg_lo(swing) < leg_lo(base), "back leg steps further out");
        assert!(leg_hi(swing) > leg_hi(base), "front leg steps further out");
        // torso and shadow rows are untouched
        for y in [150, 160, 175] {
            for x in 0..FRAME_W as usize {
                let i = (y * base.width as usize + x) * 4;
                assert_eq!(
                    base.pixels()[i..i + 4],
                    swing.pixels()[i..i + 4],
                    "row {y} col {x}"
                );
            }
        }
    }
}

#[cfg(test)]
mod typing_tests {
    use super::*;

    #[test]
    fn sheet_adds_an_arm_drop_frame_for_typing() {
        let media = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("media");
        let sheet = Sheet::load(&media);
        let alt = sheet.get("typing-press").expect("derived typing frame");
        let base = sheet.get("typing").unwrap();
        // the arm blob (dark orange) sat at y 153..160; in the derived frame
        // it lifted 1px (upstream translateY(-1px)) and its vacated bottom
        // row shows what was beneath the arm (the body colour, from y161)
        let i = ((152 * alt.width + 80) * 4) as usize;
        assert_eq!(&alt.pixels()[i..i + 3], &ARM_COLOR[..], "arm lifted up");
        assert_eq!(alt.alpha_at(80, 152), 255);
        let j = ((160 * alt.width + 80) * 4) as usize;
        assert_eq!(alt.pixels()[j..j + 3], [222, 136, 109], "vacated row shows the body below");
        // base frame untouched
        assert_eq!(base.alpha_at(80, 153), 255);
    }
}
