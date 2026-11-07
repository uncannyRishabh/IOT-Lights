//! Lighting Choreography, Color Palettes, and Hardware Rate Limiting.

use std::collections::VecDeque;
use std::time::{Duration, Instant};
use tuya_bulb::BulbColor;
use crate::dsp::BeatEvent;
use serde::{Deserialize, Serialize};

/// Curated color palettes for dynamic music visualization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ColorPalette {
    #[default]
    Cyberpunk,
    Synthwave,
    ClubRave,
    Vaporwave,
    AmbientChill,
    Custom,
}

impl ColorPalette {
    /// List of hues (degrees) defining the palette.
    pub fn hues(&self) -> &'static [f32] {
        match self {
            ColorPalette::Cyberpunk => &[180.0, 300.0, 270.0, 210.0, 330.0],
            ColorPalette::Synthwave => &[25.0, 330.0, 280.0, 45.0, 310.0],
            ColorPalette::ClubRave => &[0.0, 120.0, 240.0, 60.0, 180.0, 300.0],
            ColorPalette::Vaporwave => &[320.0, 180.0, 260.0, 30.0, 200.0],
            ColorPalette::AmbientChill => &[140.0, 200.0, 40.0, 170.0, 220.0],
            ColorPalette::Custom => &[35.0, 345.0, 235.0, 155.0],
        }
    }
}

/// Transition mode: Stem-Aware Auto, Continuous Gradual, or Instant Jump.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum TransitionMode {
    #[default]
    Auto,
    Gradual,
    Off,
}

/// Physical bulb ambience behavior modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum BulbAmbienceMode {
    /// Club / Strobe Bias: low floor (1-5%), high contrast bursts, rapid decay.
    #[default]
    DarkMode,
    /// Ambient / Room-Fill Bias: high floor (50-60%), room never goes dark, gentle swells.
    LightMode,
}

/// A choreographed bulb command output ready for transmission.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChoreographedCommand {
    pub color: BulbColor,
    pub brightness: u32,  // 10 to 1000
    pub temp: u32,        // 0 to 1000
    pub is_drop: bool,
    pub is_kick: bool,
    pub is_snare: bool,
    pub is_gradual: bool,
}

/// Choreography and hardware-protection rate limiting engine.
pub struct Choreographer {
    pub palette: ColorPalette,
    pub custom_palette: Vec<BulbColor>,
    pub color_index: usize,
    pub last_command_time: Instant,
    pub min_interval: Duration,
    pub last_beat_time: Instant,
    pub min_beat_interval: Duration,
    pub last_color_change_time: Instant,
    pub stagnation_timeout: Duration,
    pub target_hue: f32,
    pub target_sat: f32,
    pub target_val: f32,
    pub current_hue: f32,
    pub current_sat: f32,
    pub current_val: f32,
    pub current_brightness: u32,
    pub ambience_mode: BulbAmbienceMode,
    pub master_brightness: f32,
    pub transition_mode: TransitionMode,
    pub invert_auto_gradual: bool,
    pub transition_speed: f32,
    pub color_frequency: f32,
    pub live_color_frequency: f32,
    pub onset_history: VecDeque<Instant>,
    pub quiet_frames: u32,
    pub rest_sent: bool,
}

impl Choreographer {
    pub fn new(max_fps: u32, palette: ColorPalette) -> Self {
        let fps = max_fps.clamp(1, 25);
        let hues = palette.hues();
        let initial_hue = hues[0];
        let now = Instant::now() - Duration::from_secs(2);

        Self {
            palette,
            custom_palette: vec![
                BulbColor::from_rgb(245, 158, 11),  // Amber #F59E0B
                BulbColor::from_rgb(244, 63, 94),   // Coral #F43F5E
                BulbColor::from_rgb(99, 102, 241),  // Indigo #6366F1
                BulbColor::from_rgb(16, 185, 129),  // Emerald #10B981
            ],
            color_index: 0,
            last_command_time: now,
            min_interval: Duration::from_millis(1000 / fps as u64),
            last_beat_time: now,
            min_beat_interval: Duration::from_millis(160),
            last_color_change_time: now,
            stagnation_timeout: Duration::from_millis(750),
            target_hue: initial_hue,
            target_sat: 1.0,
            target_val: 1.0,
            current_hue: initial_hue,
            current_sat: 1.0,
            current_val: 1.0,
            current_brightness: 800,
            ambience_mode: BulbAmbienceMode::DarkMode,
            master_brightness: 1.0,
            transition_mode: TransitionMode::Auto,
            invert_auto_gradual: false,
            transition_speed: 0.65,
            color_frequency: 0.70,
            live_color_frequency: 0.70,
            onset_history: VecDeque::with_capacity(16),
            quiet_frames: 0,
            rest_sent: false,
        }
    }

    /// Applies mode-specific chromatic styling so DarkMode and LightMode never converge to the same color.
    pub fn apply_ambience_chromatic_profile(
        &self,
        raw_hue: f32,
        raw_sat: f32,
        raw_val: f32,
        energy: f32,
    ) -> (f32, f32, f32) {
        match self.ambience_mode {
            BulbAmbienceMode::DarkMode => {
                // Deep Club Neon: hyper-saturated (0.92 - 1.0), crisp deep-club hue snap
                let sat = (raw_sat * 1.08).clamp(0.92, 1.0);
                let val = (raw_val * (0.75 + 0.25 * energy)).clamp(0.65, 1.0);
                (raw_hue.rem_euclid(360.0), sat, val)
            }
            BulbAmbienceMode::LightMode => {
                // Room Fill Pastel/Warm: compress harsh blues/reds toward warm golden/teal/lavender
                // and cap saturation to 0.38..0.66 so bulbs emit airy pastel daylight tones
                let warm_shifted_hue = {
                    let diff_to_warm = ((38.0 - raw_hue + 540.0).rem_euclid(360.0)) - 180.0;
                    (raw_hue + diff_to_warm * 0.22).rem_euclid(360.0)
                };
                let sat = (raw_sat * (0.45 + 0.18 * energy)).clamp(0.38, 0.66);
                let val = (raw_val * 0.96).clamp(0.88, 1.0);
                (warm_shifted_hue, sat, val)
            }
        }
    }

    pub fn effective_color_frequency(&self) -> f32 {
        if self.transition_mode == TransitionMode::Auto {
            self.live_color_frequency
        } else {
            self.color_frequency
        }
    }

    pub fn effective_color_interval_ms(&self) -> u32 {
        self.min_beat_interval.as_millis() as u32
    }

    pub fn set_palette(&mut self, palette: ColorPalette) {
        if self.palette != palette {
            self.palette = palette;
            self.color_index = 0;
            if palette == ColorPalette::Custom && !self.custom_palette.is_empty() {
                let first = self.custom_palette[0];
                self.target_hue = first.h;
                self.target_sat = first.s;
                self.target_val = first.v;
            } else {
                self.target_hue = palette.hues()[0];
                self.target_sat = 1.0;
                self.target_val = 1.0;
            }
            self.last_color_change_time = Instant::now();
        }
    }

    pub fn set_custom_palette(&mut self, colors: Vec<BulbColor>) {
        if !colors.is_empty() {
            self.custom_palette = colors;
            if self.palette == ColorPalette::Custom {
                self.color_index %= self.custom_palette.len();
                let c = self.custom_palette[self.color_index];
                self.target_hue = c.h;
                self.target_sat = c.s;
                self.target_val = c.v;
            }
        }
    }

    pub fn set_transition_mode(&mut self, mode: TransitionMode) {
        self.transition_mode = mode;
    }

    pub fn transition_mode(&self) -> TransitionMode {
        self.transition_mode
    }

    pub fn set_invert_auto_gradual(&mut self, inverted: bool) {
        self.invert_auto_gradual = inverted;
    }

    pub fn set_transition_speed(&mut self, speed: f32) {
        self.transition_speed = speed.clamp(0.10, 1.00);
    }

    pub fn set_color_frequency(&mut self, freq: f32) {
        self.color_frequency = freq.clamp(0.10, 1.00);
        let min_interval_ms = (320.0 - 225.0 * self.color_frequency).round().max(75.0) as u64;
        let stag_ms = (1600.0 - 1220.0 * self.color_frequency).round().max(250.0) as u64;
        self.min_beat_interval = Duration::from_millis(min_interval_ms);
        self.stagnation_timeout = Duration::from_millis(stag_ms);
    }

    pub fn set_ambience_mode(&mut self, mode: BulbAmbienceMode) {
        self.ambience_mode = mode;
    }

    pub fn ambience_mode(&self) -> BulbAmbienceMode {
        self.ambience_mode
    }

    pub fn set_master_brightness(&mut self, brightness: f32) {
        self.master_brightness = brightness.clamp(0.01, 1.0);
    }

    pub fn master_brightness(&self) -> f32 {
        self.master_brightness
    }

    pub fn set_smooth_transitions(&mut self, smooth: bool) {
        self.transition_mode = if smooth {
            TransitionMode::Gradual
        } else {
            TransitionMode::Off
        };
    }

    pub fn smooth_transitions(&self) -> bool {
        self.transition_mode != TransitionMode::Off
    }

    pub fn current_color(&self) -> BulbColor {
        BulbColor::from_hsv(self.current_hue, self.current_sat, self.current_val)
    }

    fn advance_color(&mut self) {
        if self.palette == ColorPalette::Custom && !self.custom_palette.is_empty() {
            self.color_index = (self.color_index + 1) % self.custom_palette.len();
            let c = self.custom_palette[self.color_index];
            self.target_hue = c.h;
            self.target_sat = c.s;
            self.target_val = c.v;
        } else {
            let hues = self.palette.hues();
            self.color_index = (self.color_index + 1) % hues.len();
            self.target_hue = hues[self.color_index];
            self.target_sat = 1.0;
            self.target_val = 1.0;
        }
    }

    pub fn handle_event(&mut self, event: &BeatEvent) -> Option<ChoreographedCommand> {
        let now = Instant::now();

        if event.is_kick || event.is_snare || event.is_vocal_onset {
            self.onset_history.push_back(now);
            if self.onset_history.len() > 16 {
                self.onset_history.pop_front();
            }
        }
        self.onset_history.retain(|&t| now.duration_since(t).as_secs_f32() <= 2.5);

        let density = (self.onset_history.len() as f32 / 2.5).min(6.0);
        let e_inst = event.instrument_intensity;
        let e_flux = (event.spectral_flux / 400.0).clamp(0.0, 1.0);
        let f_target = (0.15 + 0.45 * (density / 4.2).min(1.0) + 0.25 * e_inst + 0.15 * e_flux).clamp(0.12, 0.98);

        let alpha_track = if f_target > self.live_color_frequency {
            0.45
        } else {
            0.06
        };
        self.live_color_frequency = (1.0 - alpha_track) * self.live_color_frequency + alpha_track * f_target;

        if event.overall_energy < 0.15 && !event.is_kick && !event.is_snare && !event.is_vocal_onset {
            self.quiet_frames = self.quiet_frames.saturating_add(1);
        } else {
            self.quiet_frames = 0;
            self.rest_sent = false;
        }

        if now.duration_since(self.last_command_time) < self.min_interval {
            return None;
        }

        let effective_freq = if self.transition_mode == TransitionMode::Auto {
            self.live_color_frequency
        } else {
            self.color_frequency
        };
        let min_beat_ms = (320.0 - 225.0 * effective_freq).round().max(75.0) as u64;
        let stag_ms = (1600.0 - 1220.0 * effective_freq).round().max(250.0) as u64;
        self.min_beat_interval = Duration::from_millis(min_beat_ms);
        self.stagnation_timeout = Duration::from_millis(stag_ms);

        if event.overall_energy > 0.28
            && now.duration_since(self.last_color_change_time) >= self.stagnation_timeout
        {
            self.advance_color();
            self.last_color_change_time = now;
        }

        let is_gradual = match self.transition_mode {
            TransitionMode::Off => false,
            TransitionMode::Gradual => true,
            TransitionMode::Auto => {
                if event.instrument_presence.has_instruments {
                    self.invert_auto_gradual
                } else {
                    !self.invert_auto_gradual
                }
            }
        };

        let mut is_drop = false;
        let mut is_kick = false;
        let mut is_snare = false;
        let raw_brightness: f32;

        if event.is_kick {
            self.last_command_time = now;
            is_kick = true;

            is_drop = self.quiet_frames >= 15 && event.bass_intensity > 0.6;

            if now.duration_since(self.last_beat_time) >= self.min_beat_interval {
                self.advance_color();
                self.last_beat_time = now;
                self.last_color_change_time = now;
            }

            raw_brightness = match self.ambience_mode {
                BulbAmbienceMode::DarkMode => 350.0 + (event.bass_intensity * 650.0),
                BulbAmbienceMode::LightMode => 650.0 + (event.bass_intensity * 300.0),
            };
        } else if event.is_snare {
            self.last_command_time = now;
            is_snare = true;

            if is_gradual {
                if now.duration_since(self.last_beat_time) >= self.min_beat_interval {
                    self.advance_color();
                    self.last_beat_time = now;
                    self.last_color_change_time = now;
                }
            } else {
                self.target_hue = (self.current_hue + 180.0).rem_euclid(360.0);
            }

            raw_brightness = match self.ambience_mode {
                BulbAmbienceMode::DarkMode => 250.0 + (event.snare_intensity * 600.0),
                BulbAmbienceMode::LightMode => 600.0 + (event.snare_intensity * 320.0),
            };
        } else if event.is_vocal_onset {
            self.last_command_time = now;
            if now.duration_since(self.last_beat_time) >= self.min_beat_interval {
                self.advance_color();
                self.last_beat_time = now;
                self.last_color_change_time = now;
            }

            raw_brightness = match self.ambience_mode {
                BulbAmbienceMode::DarkMode => 200.0 + (event.vocal_intensity * 500.0),
                BulbAmbienceMode::LightMode => 580.0 + (event.vocal_intensity * 300.0),
            };
        } else if event.overall_energy > 0.45 {
            self.last_command_time = now;
            raw_brightness = match self.ambience_mode {
                BulbAmbienceMode::DarkMode => 100.0 + (event.overall_energy * 350.0),
                BulbAmbienceMode::LightMode => 580.0 + (event.overall_energy * 300.0),
            };
        } else if self.quiet_frames >= 30 && !self.rest_sent {
            self.last_command_time = now;
            self.rest_sent = true;
            self.target_sat = 0.50;
            self.target_val = 0.50;

            raw_brightness = match self.ambience_mode {
                BulbAmbienceMode::DarkMode => 15.0,
                BulbAmbienceMode::LightMode => 580.0,
            };
        } else if self.quiet_frames < 30 {
            self.last_command_time = now;
            let floor = match self.ambience_mode {
                BulbAmbienceMode::DarkMode => 15.0,
                BulbAmbienceMode::LightMode => 580.0,
            };
            let decay = match self.ambience_mode {
                BulbAmbienceMode::DarkMode => 0.74,
                BulbAmbienceMode::LightMode => 0.94,
            };
            raw_brightness = (self.current_brightness as f32 * decay).max(floor);
        } else {
            return None;
        }

        if is_gradual {
            let diff = ((self.target_hue - self.current_hue + 540.0).rem_euclid(360.0)) - 180.0;
            let base_alpha = (0.18 + 0.68 * self.transition_speed).clamp(0.15, 0.92);
            let alpha = if diff.abs() > 65.0 {
                (base_alpha * 1.35).min(0.96)
            } else {
                base_alpha
            };

            self.current_hue = (self.current_hue + diff * alpha).rem_euclid(360.0);
            self.current_sat += (self.target_sat - self.current_sat) * alpha;
            self.current_val += (self.target_val - self.current_val) * alpha;
        } else {
            self.current_hue = self.target_hue;
            self.current_sat = self.target_sat;
            self.current_val = self.target_val;
        }

        let scaled_brightness = ((raw_brightness * self.master_brightness).round().clamp(10.0, 1000.0)) as u32;
        self.current_brightness = scaled_brightness;

        let (final_hue, final_sat, final_val) = self.apply_ambience_chromatic_profile(
            self.current_hue,
            self.current_sat,
            self.current_val,
            event.overall_energy,
        );
        let color = BulbColor::from_hsv(final_hue, final_sat, final_val);

        Some(ChoreographedCommand {
            color,
            brightness: self.current_brightness,
            temp: 500,
            is_drop,
            is_kick,
            is_snare,
            is_gradual,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::InstrumentPresence;

    #[test]
    fn auto_transition_mode_disables_gradual_on_instruments_and_enables_on_vocals() {
        let mut ch = Choreographer::new(20, ColorPalette::Cyberpunk);
        ch.set_transition_mode(TransitionMode::Auto);
        ch.set_invert_auto_gradual(false);

        let vocal_only_event = BeatEvent {
            is_kick: false,
            is_snare: false,
            is_vocal_onset: true,
            bass_intensity: 0.02,
            snare_intensity: 0.10,
            instrument_intensity: 0.05,
            vocal_intensity: 0.75,
            overall_energy: 0.35,
            spectral_flux: 45.0,
            current_db: -24.0,
            gate_open: true,
            instrument_presence: InstrumentPresence {
                has_instruments: false,
                instrument_score: 0.10,
            },
        };

        let cmd_vocal = ch.handle_event(&vocal_only_event).expect("vocal command");
        assert!(cmd_vocal.is_gradual, "Vocal-only stem in default Auto mode must enable gradual LERP");

        ch.last_command_time = Instant::now() - Duration::from_millis(200);

        let full_band_event = BeatEvent {
            is_kick: true,
            is_snare: true,
            is_vocal_onset: false,
            bass_intensity: 0.92,
            snare_intensity: 0.80,
            instrument_intensity: 0.90,
            vocal_intensity: 0.15,
            overall_energy: 0.95,
            spectral_flux: 310.0,
            current_db: -9.5,
            gate_open: true,
            instrument_presence: InstrumentPresence {
                has_instruments: true,
                instrument_score: 0.88,
            },
        };

        let cmd_band = ch.handle_event(&full_band_event).expect("full band command");
        assert!(!cmd_band.is_gradual, "Full instruments/beats in Auto mode must turn Gradual Mode OFF (instant jump)");
    }

    #[test]
    fn rate_limiter_drops_rapid_bursts() {
        let mut ch = Choreographer::new(10, ColorPalette::Cyberpunk);
        let kick = BeatEvent {
            is_kick: true,
            is_snare: false,
            is_vocal_onset: false,
            bass_intensity: 0.8,
            snare_intensity: 0.1,
            instrument_intensity: 0.8,
            vocal_intensity: 0.05,
            overall_energy: 0.5,
            spectral_flux: 150.0,
            current_db: -12.0,
            gate_open: true,
            instrument_presence: InstrumentPresence {
                has_instruments: true,
                instrument_score: 0.8,
            },
        };

        let cmd1 = ch.handle_event(&kick);
        assert!(cmd1.is_some());

        let cmd2 = ch.handle_event(&kick);
        assert!(cmd2.is_none());
    }

    #[test]
    fn anti_stagnation_forces_palette_rotation() {
        let mut ch = Choreographer::new(20, ColorPalette::Cyberpunk);
        let initial_hue = ch.current_hue;

        let energetic_no_kick = BeatEvent {
            is_kick: false,
            is_snare: false,
            is_vocal_onset: false,
            bass_intensity: 0.3,
            snare_intensity: 0.3,
            instrument_intensity: 0.6,
            vocal_intensity: 0.2,
            overall_energy: 0.7,
            spectral_flux: 250.0,
            current_db: -14.0,
            gate_open: true,
            instrument_presence: InstrumentPresence {
                has_instruments: true,
                instrument_score: 0.6,
            },
        };

        ch.last_color_change_time = Instant::now() - Duration::from_millis(2000);
        ch.last_command_time = Instant::now() - Duration::from_millis(200);

        let cmd = ch.handle_event(&energetic_no_kick);
        assert!(cmd.is_some());
        assert_ne!(ch.target_hue, initial_hue, "Stagnant loud audio should advance target hue");
    }

    #[test]
    fn custom_palette_cycles_through_user_swatches() {
        let mut ch = Choreographer::new(20, ColorPalette::Custom);
        let custom_swatches = vec![
            BulbColor::from_rgb(255, 0, 0),
            BulbColor::from_rgb(0, 255, 0),
        ];
        ch.set_custom_palette(custom_swatches);
        assert_eq!(ch.custom_palette.len(), 2);
    }

    #[test]
    fn auto_color_change_frequency_adapts_to_fast_beats_and_slow_passages() {
        let mut ch = Choreographer::new(25, ColorPalette::Cyberpunk);
        ch.set_transition_mode(TransitionMode::Auto);
        let initial_freq = ch.effective_color_frequency();

        // Feed rapid kicks
        let fast_kick = BeatEvent {
            is_kick: true,
            is_snare: false,
            is_vocal_onset: false,
            bass_intensity: 0.95,
            snare_intensity: 0.1,
            instrument_intensity: 0.9,
            vocal_intensity: 0.0,
            overall_energy: 0.9,
            spectral_flux: 350.0,
            current_db: -10.0,
            gate_open: true,
            instrument_presence: InstrumentPresence {
                has_instruments: true,
                instrument_score: 0.9,
            },
        };

        for _ in 0..8 {
            ch.last_command_time = Instant::now() - Duration::from_millis(50);
            ch.handle_event(&fast_kick);
        }

        let fast_freq = ch.effective_color_frequency();
        assert!(
            fast_freq > initial_freq,
            "Fast kicks must accelerate live_color_frequency, got {} vs initial {}",
            fast_freq,
            initial_freq
        );
        assert!(
            ch.effective_color_interval_ms() <= 160,
            "Fast beats should reduce color interval, got {}ms",
            ch.effective_color_interval_ms()
        );
    }

    #[test]
    fn chromatic_separation_dark_mode_vs_light_mode_never_converge() {
        let mut ch_dark = Choreographer::new(20, ColorPalette::Synthwave);
        ch_dark.set_ambience_mode(BulbAmbienceMode::DarkMode);

        let mut ch_light = Choreographer::new(20, ColorPalette::Synthwave);
        ch_light.set_ambience_mode(BulbAmbienceMode::LightMode);

        let raw_h = 240.0; // Cool Blue
        let raw_s = 1.0;
        let raw_v = 1.0;
        let energy = 0.8;

        let (dark_h, dark_s, _dark_v) = ch_dark.apply_ambience_chromatic_profile(raw_h, raw_s, raw_v, energy);
        let (light_h, light_s, _light_v) = ch_light.apply_ambience_chromatic_profile(raw_h, raw_s, raw_v, energy);

        // DarkMode is hyper-saturated neon (>= 0.92)
        assert!(dark_s >= 0.92, "DarkMode saturation must be high neon, got {}", dark_s);

        // LightMode desaturates into soft pastel (0.38 - 0.66) and shifts warm
        assert!(
            light_s <= 0.66 && light_s >= 0.38,
            "LightMode saturation must be pastel (0.38..0.66), got {}",
            light_s
        );
        assert_ne!(
            dark_h, light_h,
            "LightMode must warm-shift cool blues away from pure neon"
        );
    }
}

