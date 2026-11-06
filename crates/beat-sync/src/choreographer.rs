//! Lighting Choreography, Color Palettes, and Hardware Rate Limiting.

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
        }
    }
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
    palette: ColorPalette,
    color_index: usize,
    last_command_time: Instant,
    min_interval: Duration,
    // Beat quantization & Anti-stagnation
    last_beat_time: Instant,
    min_beat_interval: Duration,
    last_color_change_time: Instant,
    stagnation_timeout: Duration,
    // Interpolation state
    target_hue: f32,
    target_sat: f32,
    target_val: f32,
    current_hue: f32,
    current_sat: f32,
    current_val: f32,
    current_brightness: u32,
    // Smart Lighting Knobs
    ambience_mode: BulbAmbienceMode,
    master_brightness: f32,
    smooth_transitions: bool,
    quiet_frames: u32,
    rest_sent: bool,
}

impl Choreographer {
    /// Create a new choreographer with specified max updates per second (typically 12-15 Hz for Tuya bulbs).
    pub fn new(max_fps: u32, palette: ColorPalette) -> Self {
        let fps = max_fps.clamp(1, 20);
        let hues = palette.hues();
        let initial_hue = hues[0];
        let now = Instant::now() - Duration::from_secs(2);

        Self {
            palette,
            color_index: 0,
            last_command_time: now,
            min_interval: Duration::from_millis(1000 / fps as u64),
            last_beat_time: now,
            min_beat_interval: Duration::from_millis(260),
            last_color_change_time: now,
            stagnation_timeout: Duration::from_millis(1600),
            target_hue: initial_hue,
            target_sat: 1.0,
            target_val: 1.0,
            current_hue: initial_hue,
            current_sat: 1.0,
            current_val: 1.0,
            current_brightness: 800,
            ambience_mode: BulbAmbienceMode::DarkMode,
            master_brightness: 1.0,
            smooth_transitions: true,
            quiet_frames: 0,
            rest_sent: false,
        }
    }

    /// Change active color palette.
    pub fn set_palette(&mut self, palette: ColorPalette) {
        if self.palette != palette {
            self.palette = palette;
            self.color_index = 0;
            self.target_hue = palette.hues()[0];
            self.last_color_change_time = Instant::now();
        }
    }

    /// Set bulb ambience mode (Dark / Club vs Light / Ambient).
    pub fn set_ambience_mode(&mut self, mode: BulbAmbienceMode) {
        self.ambience_mode = mode;
    }

    pub fn ambience_mode(&self) -> BulbAmbienceMode {
        self.ambience_mode
    }

    /// Set master bulb brightness scalar (0.01 to 1.0).
    pub fn set_master_brightness(&mut self, brightness: f32) {
        self.master_brightness = brightness.clamp(0.01, 1.0);
    }

    pub fn master_brightness(&self) -> f32 {
        self.master_brightness
    }

    /// Set smooth transitions (Gradual LERP) mode.
    pub fn set_smooth_transitions(&mut self, smooth: bool) {
        self.smooth_transitions = smooth;
    }

    pub fn smooth_transitions(&self) -> bool {
        self.smooth_transitions
    }

    /// Get current interpolated color.
    pub fn current_color(&self) -> BulbColor {
        BulbColor::from_hsv(self.current_hue, self.current_sat, self.current_val)
    }

    /// Handle a detected audio beat event. Returns `Some(ChoreographedCommand)` if an update
    /// is warranted and the rate-limiter permits transmission.
    pub fn handle_event(&mut self, event: &BeatEvent) -> Option<ChoreographedCommand> {
        let now = Instant::now();

        // Track quiet frames
        if event.overall_energy < 0.15 && !event.is_kick && !event.is_snare {
            self.quiet_frames = self.quiet_frames.saturating_add(1);
        } else {
            self.quiet_frames = 0;
            self.rest_sent = false;
        }

        // Hardware safety: enforce rate limit to prevent MCU TCP buffer overflow
        if now.duration_since(self.last_command_time) < self.min_interval {
            return None;
        }

        let hues = self.palette.hues();

        // Anti-Color-Stagnation Guard:
        // If sustained loud/energetic audio plays for > 1.6s without explicit kick triggering a new color,
        // force a palette phase-shift so bulbs never stay stuck on one color.
        if event.overall_energy > 0.30
            && now.duration_since(self.last_color_change_time) >= self.stagnation_timeout
        {
            self.color_index = (self.color_index + 1) % hues.len();
            self.target_hue = hues[self.color_index];
            self.last_color_change_time = now;
        }

        let mut is_drop = false;
        let mut is_kick = false;
        let mut is_snare = false;
        let raw_brightness: f32;

        if event.is_kick {
            self.last_command_time = now;
            is_kick = true;

            // Detect build-up drop: 15+ quiet frames followed by a strong kick
            is_drop = self.quiet_frames >= 15 && event.bass_intensity > 0.6;

            // Beat-quantized target selection: cooldown window prevents micro-beat spam
            if now.duration_since(self.last_beat_time) >= self.min_beat_interval {
                self.color_index = (self.color_index + 1) % hues.len();
                self.target_hue = hues[self.color_index];
                self.last_beat_time = now;
                self.last_color_change_time = now;
            }

            self.target_sat = 1.0;
            self.target_val = 1.0;

            raw_brightness = match self.ambience_mode {
                BulbAmbienceMode::DarkMode => 350.0 + (event.bass_intensity * 650.0),
                BulbAmbienceMode::LightMode => 650.0 + (event.bass_intensity * 350.0),
            };
        } else if event.is_snare {
            self.last_command_time = now;
            is_snare = true;

            // Complementary accent flash on snare/clap hit
            if !self.smooth_transitions {
                self.target_hue = (self.current_hue + 180.0).rem_euclid(360.0);
            }
            self.target_sat = 0.88;
            self.target_val = 1.0;

            raw_brightness = match self.ambience_mode {
                BulbAmbienceMode::DarkMode => 250.0 + (event.snare_intensity * 600.0),
                BulbAmbienceMode::LightMode => 600.0 + (event.snare_intensity * 380.0),
            };
        } else if event.overall_energy > 0.50 {
            // Sustained groove between beats
            self.last_command_time = now;
            raw_brightness = match self.ambience_mode {
                BulbAmbienceMode::DarkMode => 100.0 + (event.overall_energy * 350.0),
                BulbAmbienceMode::LightMode => 550.0 + (event.overall_energy * 300.0),
            };
        } else if self.quiet_frames >= 30 && !self.rest_sent {
            // Ambient resting state on quiet passage
            self.last_command_time = now;
            self.rest_sent = true;
            self.target_sat = 0.50;
            self.target_val = 0.50;

            raw_brightness = match self.ambience_mode {
                BulbAmbienceMode::DarkMode => 25.0,
                BulbAmbienceMode::LightMode => 500.0,
            };
        } else if self.quiet_frames < 30 {
            // Smooth decay between beats to respective floor
            self.last_command_time = now;
            let floor = match self.ambience_mode {
                BulbAmbienceMode::DarkMode => 20.0,
                BulbAmbienceMode::LightMode => 520.0,
            };
            let decay = match self.ambience_mode {
                BulbAmbienceMode::DarkMode => 0.78,
                BulbAmbienceMode::LightMode => 0.92,
            };
            raw_brightness = (self.current_brightness as f32 * decay).max(floor);
        } else {
            return None;
        }

        // HSL Shortest-Arc Interpolation
        if self.smooth_transitions {
            let diff = ((self.target_hue - self.current_hue + 540.0).rem_euclid(360.0)) - 180.0;
            self.current_hue = (self.current_hue + diff * 0.32).rem_euclid(360.0);
            self.current_sat += (self.target_sat - self.current_sat) * 0.30;
            self.current_val += (self.target_val - self.current_val) * 0.30;
        } else {
            self.current_hue = self.target_hue;
            self.current_sat = self.target_sat;
            self.current_val = self.target_val;
        }

        // Apply Master Bulb Brightness Scaler
        let scaled_brightness = ((raw_brightness * self.master_brightness).round().clamp(10.0, 1000.0)) as u32;
        self.current_brightness = scaled_brightness;

        let color = BulbColor::from_hsv(self.current_hue, self.current_sat, self.current_val);

        Some(ChoreographedCommand {
            color,
            brightness: self.current_brightness,
            temp: 500,
            is_drop,
            is_kick,
            is_snare,
            is_gradual: self.smooth_transitions,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limiter_drops_rapid_bursts() {
        let mut ch = Choreographer::new(10, ColorPalette::Cyberpunk); // 100ms interval
        let kick = BeatEvent {
            is_kick: true,
            is_snare: false,
            bass_intensity: 0.8,
            snare_intensity: 0.1,
            overall_energy: 0.5,
            spectral_flux: 150.0,
        };

        // First kick should pass
        let cmd1 = ch.handle_event(&kick);
        assert!(cmd1.is_some());

        // Second immediate kick within 100ms should be dropped to protect MCU
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
            bass_intensity: 0.3,
            snare_intensity: 0.3,
            overall_energy: 0.7,
            spectral_flux: 250.0,
        };

        // Advance artificial time for anti-stagnation by manipulating time
        ch.last_color_change_time = Instant::now() - Duration::from_millis(2000);
        ch.last_command_time = Instant::now() - Duration::from_millis(200);

        let cmd = ch.handle_event(&energetic_no_kick);
        assert!(cmd.is_some());
        assert_ne!(ch.target_hue, initial_hue, "Stagnant loud audio should advance target hue");
    }

    #[test]
    fn ambience_mode_enforces_brightness_floor() {
        let mut ch_dark = Choreographer::new(20, ColorPalette::Cyberpunk);
        ch_dark.set_ambience_mode(BulbAmbienceMode::DarkMode);

        let mut ch_light = Choreographer::new(20, ColorPalette::Cyberpunk);
        ch_light.set_ambience_mode(BulbAmbienceMode::LightMode);

        let quiet = BeatEvent {
            is_kick: false,
            is_snare: false,
            bass_intensity: 0.05,
            snare_intensity: 0.05,
            overall_energy: 0.05,
            spectral_flux: 5.0,
        };

        for _ in 0..35 {
            std::thread::sleep(Duration::from_millis(55));
            ch_dark.handle_event(&quiet);
            ch_light.handle_event(&quiet);
        }

        assert!(ch_light.current_brightness >= 500, "LightMode should maintain ambient floor >= 500, got {}", ch_light.current_brightness);
        assert!(ch_dark.current_brightness <= 100, "DarkMode should decay to low club floor <= 100, got {}", ch_dark.current_brightness);
    }

    #[test]
    fn master_brightness_scales_output() {
        let mut ch = Choreographer::new(20, ColorPalette::Cyberpunk);
        ch.set_master_brightness(0.5); // 50% brightness cap

        let kick = BeatEvent {
            is_kick: true,
            is_snare: false,
            bass_intensity: 1.0,
            snare_intensity: 0.0,
            overall_energy: 0.9,
            spectral_flux: 300.0,
        };

        let cmd = ch.handle_event(&kick).expect("kick cmd");
        assert!(cmd.brightness <= 500, "50% master brightness should cap output <= 500, got {}", cmd.brightness);
    }
}
