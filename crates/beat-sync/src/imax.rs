//! IMAX Mode: Ultra-low-overhead native screen compressor, zero-black filter,
//! and Oklab Quantized Chromatic Peak + Mean-Shift Prominent Color Engine.

use std::time::Instant;
use serde::{Deserialize, Serialize};
use tuya_bulb::BulbColor;

const COMPRESSED_W: usize = 64;
const COMPRESSED_H: usize = 36;
const HUE_BINS: usize = 24;

/// Information about a detected physical display.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DisplayInfo {
    pub index: u32,
    pub display_id: u32,
    pub width: u32,
    pub height: u32,
    pub is_main: bool,
    pub name: String,
}

/// Color extraction strategy for IMAX screen sync.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ImaxColorMode {
    /// Dominant / Prominent Color: Oklab Chromatic Peak Histogram + Mean-Shift Cluster.
    /// Avoids muddy averaging of complementary colors and locks onto the primary vivid subject.
    #[default]
    Prominent,
    /// Perceptual Chroma-Weighted Ambient Average across non-black screen regions.
    AmbientAverage,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ImaxConfig {
    pub target_fps: u32,
    pub display_index: u32,
    /// Reaction responsiveness (0.10 = smooth glide, 0.90 = ultra-fast <20ms snap on cuts)
    pub smoothing: f32,
    /// Chroma boost multiplier (e.g. 1.35) so LEDs render rich cinema saturation
    pub saturation_boost: f32,
    pub min_brightness: u32,
    pub max_brightness: u32,
    pub color_mode: ImaxColorMode,
}

impl Default for ImaxConfig {
    fn default() -> Self {
        Self {
            target_fps: 20,
            display_index: 0,
            smoothing: 0.82,
            saturation_boost: 1.35,
            min_brightness: 25,
            max_brightness: 1000,
            color_mode: ImaxColorMode::Prominent,
        }
    }
}

#[derive(Clone, Copy, Default)]
struct HueBin {
    weight: f32,
    sum_l: f32,
    sum_a: f32,
    sum_b: f32,
}

pub struct ImaxEngine {
    pub cfg: ImaxConfig,
    pub cur_oklab: [f32; 3],
    pub cur_luma: f32,
    pub prev_peak_bin: usize,
    pub last_tick: Instant,
    /// Pre-allocated buffer for hardware-compressed 64x36 frame
    pub downscaled_buf: Vec<u8>,
}

impl ImaxEngine {
    pub fn new(cfg: ImaxConfig) -> Self {
        Self {
            cfg,
            cur_oklab: [0.55, 0.0, 0.0],
            cur_luma: 0.35,
            prev_peak_bin: 0,
            last_tick: Instant::now(),
            downscaled_buf: vec![0u8; COMPRESSED_W * COMPRESSED_H * 4],
        }
    }

    /// Processes a raw RGBA8 or BGRA8 screen buffer by compressing/striding to 64x36,
    /// strictly discarding ALL black/near-black pixels, and extracting the Prominent Color
    /// via Oklab Chromatic Histogram Mode + 2-Pass Mean-Shift Refinement.
    /// Returns `(BulbColor, brightness_10_to_1000, is_instant_cut)`.
    pub fn process_rgba_strided(
        &mut self,
        buf: &[u8],
        width: usize,
        height: usize,
        bytes_per_row: usize,
        is_bgra: bool,
    ) -> (BulbColor, u32) {
        let (color, brightness, _) =
            self.process_rgba_with_cut_flag(buf, width, height, bytes_per_row, is_bgra);
        (color, brightness)
    }

    pub fn process_rgba_with_cut_flag(
        &mut self,
        buf: &[u8],
        width: usize,
        height: usize,
        bytes_per_row: usize,
        is_bgra: bool,
    ) -> (BulbColor, u32, bool) {
        let now = Instant::now();
        let dt = now.duration_since(self.last_tick).as_secs_f32().clamp(0.004, 0.20);
        self.last_tick = now;

        let grid_x = COMPRESSED_W.min(width.max(1));
        let grid_y = COMPRESSED_H.min(height.max(1));
        let step_x = (width / grid_x).max(1);
        let step_y = (height / grid_y).max(1);

        let mut bins = [HueBin::default(); HUE_BINS];
        let mut neutral_bin = HueBin::default();

        // Store non-black Oklab samples for 2nd-pass Mean-Shift refinement around the prominent mode
        let mut valid_oklab: Vec<([f32; 3], f32)> = Vec::with_capacity(grid_x * grid_y);
        let mut luma_accum = 0.0f32;
        let mut non_black_count = 0u32;

        for gy in 0..grid_y {
            let y = (gy * step_y).min(height.saturating_sub(1));
            let row_offset = y * bytes_per_row;

            for gx in 0..grid_x {
                let x = (gx * step_x).min(width.saturating_sub(1));
                let idx = row_offset + x * 4;
                if idx + 2 >= buf.len() {
                    continue;
                }

                let (r, g, b) = if is_bgra {
                    (buf[idx + 2], buf[idx + 1], buf[idx])
                } else {
                    (buf[idx], buf[idx + 1], buf[idx + 2])
                };

                // STRICT BLACK & NEAR-BLACK REJECTION:
                // Completely ignore black bars, dark space, and near-black shadow pixels.
                let max_u8 = r.max(g).max(b);
                if max_u8 <= 24 {
                    continue;
                }

                let rf = r as f32 / 255.0;
                let gf = g as f32 / 255.0;
                let bf = b as f32 / 255.0;
                let luma = 0.2126 * rf + 0.7152 * gf + 0.0722 * bf;
                if luma < 0.065 {
                    continue;
                }

                luma_accum += luma;
                non_black_count += 1;

                let [ok_l, ok_a, ok_b] = srgb_to_oklab(rf, gf, bf);
                let chroma = (ok_a * ok_a + ok_b * ok_b).sqrt();

                // Salience weight strongly favors vibrant chromatic pixels over washed-out grays
                let salience = (0.04 + (chroma * 4.5).powf(1.65)) * ok_l.clamp(0.15, 0.98);
                valid_oklab.push(([ok_l, ok_a, ok_b], salience));

                if chroma < 0.028 {
                    // Near-neutral white/silver pixel
                    neutral_bin.weight += ok_l * 0.25;
                    neutral_bin.sum_l += ok_l * (ok_l * 0.25);
                    neutral_bin.sum_a += ok_a * (ok_l * 0.25);
                    neutral_bin.sum_b += ok_b * (ok_l * 0.25);
                } else {
                    let angle = ok_b.atan2(ok_a).rem_euclid(2.0 * std::f32::consts::PI);
                    let bin_idx = ((angle / (2.0 * std::f32::consts::PI)) * HUE_BINS as f32)
                        .floor() as usize
                        % HUE_BINS;

                    bins[bin_idx].weight += salience;
                    bins[bin_idx].sum_l += ok_l * salience;
                    bins[bin_idx].sum_a += ok_a * salience;
                    bins[bin_idx].sum_b += ok_b * salience;
                }
            }
        }

        // If the entire screen is pitch black, hold previous chromaticity at minimum floor brightness
        if non_black_count == 0 {
            let (r8, g8, b8) =
                oklab_to_srgb8(self.cur_oklab[0], self.cur_oklab[1], self.cur_oklab[2]);
            return (
                BulbColor::from_rgb(r8, g8, b8),
                self.cfg.min_brightness.clamp(10, 1000),
                false,
            );
        }

        let target_luma = (luma_accum / non_black_count as f32).clamp(0.05, 1.0);

        let (tl, ta, tb) = match self.cfg.color_mode {
            ImaxColorMode::Prominent => {
                // 1. Parzen 3-bin circular smoothing across the 24 hue bins to find the true Chromatic Peak
                let mut best_bin = 0usize;
                let mut best_score = -1.0f32;
                let mut total_chroma_weight = 0.0f32;

                for i in 0..HUE_BINS {
                    total_chroma_weight += bins[i].weight;
                    let prev = bins[(i + HUE_BINS - 1) % HUE_BINS].weight;
                    let curr = bins[i].weight;
                    let next = bins[(i + 1) % HUE_BINS].weight;
                    let mut smoothed = 0.25 * prev + 0.50 * curr + 0.25 * next;

                    // Subtle 8% hysteresis bonus for current peak to prevent 50/50 split flicker
                    if i == self.prev_peak_bin {
                        smoothed *= 1.08;
                    }

                    if smoothed > best_score {
                        best_score = smoothed;
                        best_bin = i;
                    }
                }

                if total_chroma_weight > 1e-4 && best_score > neutral_bin.weight * 0.25 {
                    self.prev_peak_bin = best_bin;
                    // Seed centroid from the winning peak bin + its immediate +-1 neighbors
                    let mut seed_w = 0.0f32;
                    let mut seed_l = 0.0f32;
                    let mut seed_a = 0.0f32;
                    let mut seed_b = 0.0f32;
                    for offset in [HUE_BINS - 1, 0, 1] {
                        let b = &bins[(best_bin + offset) % HUE_BINS];
                        seed_w += b.weight;
                        seed_l += b.sum_l;
                        seed_a += b.sum_a;
                        seed_b += b.sum_b;
                    }
                    let mut c_l = seed_l / seed_w.max(1e-5);
                    let mut c_a = seed_a / seed_w.max(1e-5);
                    let mut c_b = seed_b / seed_w.max(1e-5);

                    // 2. Second-Pass Oklab Mean-Shift Refinement within radius 0.16 of the seed mode
                    let mut ref_w = 0.0f32;
                    let mut ref_l = 0.0f32;
                    let mut ref_a = 0.0f32;
                    let mut ref_b = 0.0f32;
                    for &(ok, sal) in &valid_oklab {
                        let da = ok[1] - c_a;
                        let db = ok[2] - c_b;
                        let dist_ab_sq = da * da + db * db;
                        if dist_ab_sq < 0.025 {
                            let kernel = (-dist_ab_sq * 60.0).exp() * sal;
                            ref_w += kernel;
                            ref_l += ok[0] * kernel;
                            ref_a += ok[1] * kernel;
                            ref_b += ok[2] * kernel;
                        }
                    }
                    if ref_w > 1e-4 {
                        c_l = ref_l / ref_w;
                        c_a = ref_a / ref_w;
                        c_b = ref_b / ref_w;
                    }

                    (
                        c_l.clamp(0.45, 0.92),
                        c_a * self.cfg.saturation_boost,
                        c_b * self.cfg.saturation_boost,
                    )
                } else if neutral_bin.weight > 1e-4 {
                    (
                        (neutral_bin.sum_l / neutral_bin.weight).clamp(0.5, 0.95),
                        0.0,
                        0.0,
                    )
                } else {
                    (0.55, 0.0, 0.0)
                }
            }
            ImaxColorMode::AmbientAverage => {
                let mut sum_l = 0.0f32;
                let mut sum_a = 0.0f32;
                let mut sum_b = 0.0f32;
                let mut sum_w = 0.0f32;
                for &(ok, sal) in &valid_oklab {
                    sum_l += ok[0] * sal;
                    sum_a += ok[1] * sal;
                    sum_b += ok[2] * sal;
                    sum_w += sal;
                }
                if sum_w > 1e-4 {
                    (
                        (sum_l / sum_w).clamp(0.40, 0.92),
                        (sum_a / sum_w) * self.cfg.saturation_boost,
                        (sum_b / sum_w) * self.cfg.saturation_boost,
                    )
                } else {
                    (0.55, 0.0, 0.0)
                }
            }
        };

        // Ultra-fast scene-change detection & minimal transition time
        let delta_e = ((tl - self.cur_oklab[0]).powi(2)
            + (ta - self.cur_oklab[1]).powi(2)
            + (tb - self.cur_oklab[2]).powi(2))
        .sqrt();
        let luma_jump = (target_luma - self.cur_luma).abs();
        let is_cut = delta_e > 0.11 || luma_jump > 0.18;

        // High responsiveness: instant snap on camera cuts / color changes, tight tracking otherwise
        let responsiveness = self.cfg.smoothing.clamp(0.10, 1.0);
        let alpha = if is_cut {
            (0.78 + 0.21 * responsiveness).clamp(0.80, 0.99)
        } else {
            let lambda = 14.0 + 36.0 * responsiveness;
            (1.0 - (-lambda * dt).exp()).clamp(0.35, 0.95)
        };

        self.cur_oklab[0] += (tl - self.cur_oklab[0]) * alpha;
        self.cur_oklab[1] += (ta - self.cur_oklab[1]) * alpha;
        self.cur_oklab[2] += (tb - self.cur_oklab[2]) * alpha;
        self.cur_luma += (target_luma - self.cur_luma) * alpha;

        let (r8, g8, b8) = oklab_to_srgb8(self.cur_oklab[0], self.cur_oklab[1], self.cur_oklab[2]);
        let color = BulbColor::from_rgb(r8, g8, b8);

        let norm_b = self.cur_luma.clamp(0.0, 1.0).powf(0.58);
        let span = (self.cfg.max_brightness.saturating_sub(self.cfg.min_brightness)) as f32;
        let brightness = (self.cfg.min_brightness as f32 + norm_b * span)
            .round()
            .clamp(10.0, 1000.0) as u32;

        (color, brightness, is_cut)
    }
}

#[cfg(target_os = "macos")]
pub mod macos_capture {
    use super::*;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CGPoint {
        x: f64,
        y: f64,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CGSize {
        width: f64,
        height: f64,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct CGRect {
        origin: CGPoint,
        size: CGSize,
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CGMainDisplayID() -> u32;
        fn CGGetActiveDisplayList(
            max_displays: u32,
            active_displays: *mut u32,
            display_count: *mut u32,
        ) -> i32;
        fn CGDisplayPixelsWide(display: u32) -> usize;
        fn CGDisplayPixelsHigh(display: u32) -> usize;
        fn CGDisplayCreateImage(display: u32) -> *mut std::ffi::c_void;
        fn CGColorSpaceCreateDeviceRGB() -> *mut std::ffi::c_void;
        fn CGColorSpaceRelease(space: *mut std::ffi::c_void);
        fn CGBitmapContextCreate(
            data: *mut std::ffi::c_void,
            width: usize,
            height: usize,
            bits_per_component: usize,
            bytes_per_row: usize,
            space: *mut std::ffi::c_void,
            bitmap_info: u32,
        ) -> *mut std::ffi::c_void;
        fn CGContextDrawImage(
            c: *mut std::ffi::c_void,
            rect: CGRect,
            image: *mut std::ffi::c_void,
        );
        fn CGContextRelease(c: *mut std::ffi::c_void);
        fn CFRelease(cf: *mut std::ffi::c_void);
    }

    /// Enumerates all active displays connected to the Mac.
    pub fn list_displays() -> Vec<DisplayInfo> {
        unsafe {
            let mut ids = [0u32; 16];
            let mut count = 0u32;
            let err = CGGetActiveDisplayList(16, ids.as_mut_ptr(), &mut count);
            let main_id = CGMainDisplayID();

            if err != 0 || count == 0 {
                let w = CGDisplayPixelsWide(main_id) as u32;
                let h = CGDisplayPixelsHigh(main_id) as u32;
                return vec![DisplayInfo {
                    index: 0,
                    display_id: main_id,
                    width: w,
                    height: h,
                    is_main: true,
                    name: format!("Display #0 (Main — {}×{})", w, h),
                }];
            }

            let mut list = Vec::with_capacity(count as usize);
            for i in 0..(count as usize) {
                let id = ids[i];
                let w = CGDisplayPixelsWide(id) as u32;
                let h = CGDisplayPixelsHigh(id) as u32;
                let is_main = id == main_id;
                let role = if is_main { "Main" } else { "External" };
                list.push(DisplayInfo {
                    index: i as u32,
                    display_id: id,
                    width: w,
                    height: h,
                    is_main,
                    name: format!("Display #{} ({} — {}×{})", i, role, w, h),
                });
            }
            list
        }
    }

    /// Hardware-compressed 64x36 frame capture + Prominent Color extraction.
    /// Downscales the full 4K/5K screen directly inside CoreGraphics into `engine.downscaled_buf`
    /// without allocating a multi-megabyte full-resolution copy in RAM.
    pub fn capture_screen_frame(
        display_index_or_id: u32,
        engine: &mut ImaxEngine,
    ) -> Option<(BulbColor, u32, bool)> {
        unsafe {
            let displays = list_displays();
            let target_id = if let Some(d) = displays.iter().find(|d| d.index == display_index_or_id)
            {
                d.display_id
            } else if display_index_or_id == 0 {
                CGMainDisplayID()
            } else {
                display_index_or_id
            };

            let image = CGDisplayCreateImage(target_id);
            if image.is_null() {
                return None;
            }

            let color_space = CGColorSpaceCreateDeviceRGB();
            // kCGImageAlphaPremultipliedLast | kCGBitmapByteOrder32Big = 1 | (2 << 12) = 8193 (RGBA8)
            let bitmap_info: u32 = 1;
            let bytes_per_row = COMPRESSED_W * 4;

            let ctx = CGBitmapContextCreate(
                engine.downscaled_buf.as_mut_ptr() as *mut std::ffi::c_void,
                COMPRESSED_W,
                COMPRESSED_H,
                8,
                bytes_per_row,
                color_space,
                bitmap_info,
            );

            if ctx.is_null() {
                CGColorSpaceRelease(color_space);
                CFRelease(image);
                return None;
            }

            let rect = CGRect {
                origin: CGPoint { x: 0.0, y: 0.0 },
                size: CGSize {
                    width: COMPRESSED_W as f64,
                    height: COMPRESSED_H as f64,
                },
            };

            // Hardware/SIMD downscale from native resolution directly into 64x36 buffer
            CGContextDrawImage(ctx, rect, image);

            CGContextRelease(ctx);
            CGColorSpaceRelease(color_space);
            CFRelease(image);

            let buf_copy = engine.downscaled_buf.clone();
            let res = engine.process_rgba_with_cut_flag(
                &buf_copy,
                COMPRESSED_W,
                COMPRESSED_H,
                bytes_per_row,
                false,
            );
            Some(res)
        }
    }
}

#[cfg(target_os = "macos")]
pub use macos_capture as screen_capture;

#[cfg(any(target_os = "linux", target_os = "windows"))]
pub mod screen_capture {
    use super::*;

    pub fn list_displays() -> Vec<DisplayInfo> {
        match xcap::Monitor::all() {
            Ok(monitors) => monitors
                .into_iter()
                .enumerate()
                .map(|(idx, m)| {
                    let w = m.width();
                    let h = m.height();
                    let is_pri = m.is_primary();
                    let name = m.name().to_string();
                    DisplayInfo {
                        index: idx as u32,
                        display_id: m.id(),
                        width: w,
                        height: h,
                        is_main: is_pri,
                        name: format!(
                            "Display #{} — {} ({}×{}{})",
                            idx,
                            name,
                            w,
                            h,
                            if is_pri { ", Primary" } else { "" }
                        ),
                    }
                })
                .collect(),
            Err(_) => vec![],
        }
    }

    pub fn capture_screen_frame(
        display_idx: u32,
        engine: &mut ImaxEngine,
    ) -> Option<(BulbColor, u32, bool)> {
        let monitors = xcap::Monitor::all().ok()?;
        let monitor = monitors.into_iter().nth(display_idx as usize)?;
        let img = monitor.capture_image().ok()?;
        let w = img.width() as usize;
        let h = img.height() as usize;
        let raw = img.into_raw();
        let res = engine.process_rgba_with_cut_flag(&raw, w, h, w * 4, false);
        Some(res)
    }
}

#[cfg(any(target_os = "linux", target_os = "windows"))]
pub use screen_capture as macos_capture;

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
pub mod screen_capture {
    use super::*;

    pub fn list_displays() -> Vec<DisplayInfo> {
        vec![DisplayInfo {
            index: 0,
            display_id: 0,
            width: 1920,
            height: 1080,
            is_main: true,
            name: "Display #0 (Default — 1920×1080)".to_string(),
        }]
    }

    pub fn capture_screen_frame(
        _display_idx: u32,
        _engine: &mut ImaxEngine,
    ) -> Option<(BulbColor, u32, bool)> {
        None
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
pub use screen_capture as macos_capture;

#[inline]
fn srgb_to_oklab(r: f32, g: f32, b: f32) -> [f32; 3] {
    let lin =
        |c: f32| if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) };
    let (lr, lg, lb) = (lin(r), lin(g), lin(b));
    let l = (0.41222147 * lr + 0.53633254 * lg + 0.05144599 * lb).cbrt();
    let m = (0.21190350 * lr + 0.68069955 * lg + 0.10739696 * lb).cbrt();
    let s = (0.08830246 * lr + 0.28171884 * lg + 0.62997870 * lb).cbrt();
    [
        0.21045426 * l + 0.79361779 * m - 0.00407205 * s,
        1.97799850 * l - 2.42859221 * m + 0.45059371 * s,
        0.02590404 * l + 0.78277177 * m - 0.80867577 * s,
    ]
}

#[inline]
fn oklab_to_srgb8(l_ok: f32, a_ok: f32, b_ok: f32) -> (u8, u8, u8) {
    let l_ = l_ok + 0.39633778 * a_ok + 0.21580376 * b_ok;
    let m_ = l_ok - 0.10556135 * a_ok - 0.06385417 * b_ok;
    let s_ = l_ok - 0.08948418 * a_ok - 1.29148555 * b_ok;
    let (l, m, s) = (l_ * l_ * l_, m_ * m_ * m_, s_ * s_ * s_);
    let gamma = |v: f32| -> u8 {
        let c = v.clamp(0.0, 1.0);
        let g = if c <= 0.0031308 {
            12.92 * c
        } else {
            1.055 * c.powf(1.0 / 2.4) - 0.055
        };
        (g * 255.0).round() as u8
    };
    (
        gamma(4.0767417 * l - 3.3077116 * m + 0.2309699 * s),
        gamma(-1.2684380 * l + 2.6097574 * m - 0.3413194 * s),
        gamma(-0.0041961 * l - 0.7034186 * m + 1.7076147 * s),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn imax_prominent_color_ignores_black_and_avoids_muddy_average() {
        let mut engine = ImaxEngine::new(ImaxConfig::default());

        // Construct a 64x36 frame:
        // - 70% pitch black space / black bars (0, 0, 0)
        // - 20% dim gray rock (60, 60, 60)
        // - 10% vivid crimson-red lightsaber/explosion (250, 20, 40)
        // A naive average would produce a dark muddy gray-brown.
        // Our Prominent Color Engine must ignore all black pixels and lock onto the vivid red cluster!
        let width = 64;
        let height = 36;
        let bytes_per_row = width * 4;
        let mut buf = vec![0u8; height * bytes_per_row];

        for y in 0..height {
            for x in 0..width {
                let idx = (y * width + x) * 4;
                if y < 24 {
                    // Black space (ignored completely)
                    buf[idx] = 0;
                    buf[idx + 1] = 0;
                    buf[idx + 2] = 0;
                } else if y < 31 {
                    // Dim neutral gray rock
                    buf[idx] = 60;
                    buf[idx + 1] = 60;
                    buf[idx + 2] = 60;
                } else {
                    // Prominent vivid crimson-red subject
                    buf[idx] = 250;
                    buf[idx + 1] = 20;
                    buf[idx + 2] = 40;
                }
                buf[idx + 3] = 255;
            }
        }

        let (color, brightness, is_cut) =
            engine.process_rgba_with_cut_flag(&buf, width, height, bytes_per_row, false);
        let (r, g, b) = color.to_rgb();

        assert!(is_cut, "Initial jump to vivid crimson scene should trigger instant cut reaction");
        assert!(
            r > 190 && g < 80 && b < 90,
            "Prominent color engine must lock onto vivid crimson subject instead of gray/black average, got RGB({},{},{})",
            r, g, b
        );
        assert!(
            brightness > 250,
            "Black pixels must be ignored so brightness is not crushed, got {}",
            brightness
        );
    }
}
