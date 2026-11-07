//! DSP Beat, Instrument, and Vocal Separation Engine using Real-to-Complex FFT,
//! Online 2D Harmonic-Percussive Source Separation (HPSS), Formant Crest Analysis,
//! and Volume-Invariant Log-Spectral Flux ("SuperFlux") Beat Tracking.

use std::sync::Arc;
use num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};
use serde::{Deserialize, Serialize};

/// Temporal window length for online median Harmonic-Percussive Separation (must be odd).
const HPSS_TIME_FRAMES: usize = 7;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum AudioFocusMode {
    #[default]
    FullMix,
    VocalsOnly,
    InstrumentsOnly,
}

/// Zero-allocation Direct Form II Transposed Biquad Filter.
#[derive(Debug, Clone, Copy, Default)]
pub struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
}

impl Biquad {
    pub fn butterworth_highpass(fc: f32, fs: f32) -> Self {
        let omega = 2.0 * std::f32::consts::PI * (fc / fs).clamp(0.001, 0.48);
        let (sin_w, cos_w) = omega.sin_cos();
        let alpha = sin_w / std::f32::consts::SQRT_2;
        let a0 = 1.0 + alpha;
        Self {
            b0: ((1.0 + cos_w) * 0.5) / a0,
            b1: -(1.0 + cos_w) / a0,
            b2: ((1.0 + cos_w) * 0.5) / a0,
            a1: (-2.0 * cos_w) / a0,
            a2: (1.0 - alpha) / a0,
            z1: 0.0,
            z2: 0.0,
        }
    }

    pub fn butterworth_lowpass(fc: f32, fs: f32) -> Self {
        let omega = 2.0 * std::f32::consts::PI * (fc / fs).clamp(0.001, 0.48);
        let (sin_w, cos_w) = omega.sin_cos();
        let alpha = sin_w / std::f32::consts::SQRT_2;
        let a0 = 1.0 + alpha;
        Self {
            b0: ((1.0 - cos_w) * 0.5) / a0,
            b1: (1.0 - cos_w) / a0,
            b2: ((1.0 - cos_w) * 0.5) / a0,
            a1: (-2.0 * cos_w) / a0,
            a2: (1.0 - alpha) / a0,
            z1: 0.0,
            z2: 0.0,
        }
    }

    /// Peaking notch filter to attenuate center vocal formant band in InstrumentsOnly mode.
    pub fn vocal_notch(fc: f32, fs: f32, q: f32, gain_db: f32) -> Self {
        let a = 10.0f32.powf(gain_db / 40.0);
        let omega = 2.0 * std::f32::consts::PI * (fc / fs).clamp(0.001, 0.48);
        let (sin_w, cos_w) = omega.sin_cos();
        let alpha = sin_w / (2.0 * q);
        let a0 = 1.0 + alpha / a;
        Self {
            b0: (1.0 + alpha * a) / a0,
            b1: (-2.0 * cos_w) / a0,
            b2: (1.0 - alpha * a) / a0,
            a1: (-2.0 * cos_w) / a0,
            a2: (1.0 - alpha / a) / a0,
            z1: 0.0,
            z2: 0.0,
        }
    }

    #[inline(always)]
    pub fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }
}

/// Real-time assessment of instrument vs vocal dominance.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub struct InstrumentPresence {
    pub has_instruments: bool,
    pub instrument_score: f32,
}

/// Detected beat, instrument, and vocal transient event.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BeatEvent {
    /// Strong low-frequency kick drum or 808 transient hit
    pub is_kick: bool,
    /// Percussive snare, clap, or guitar/synth transient hit
    pub is_snare: bool,
    /// Vocal onset or sustained harmonic singing phrase in speech/formant band
    pub is_vocal_onset: bool,
    /// Normalized sub-bass energy (0.0 to 1.0)
    pub bass_intensity: f32,
    /// Normalized percussive/snare energy (0.0 to 1.0)
    pub snare_intensity: f32,
    /// Normalized total instrument energy (drums, bass, guitars, synths: 0.0 to 1.0)
    pub instrument_intensity: f32,
    /// Normalized isolated vocal harmonic formant energy (0.0 to 1.0)
    pub vocal_intensity: f32,
    /// Overall sound level + spectral dynamics envelope (0.0 to 1.0)
    pub overall_energy: f32,
    /// Raw calculated spectral flux value
    pub spectral_flux: f32,
    /// Pre-AGC frame RMS in dBFS (-65.0 dB to 0.0 dB)
    pub current_db: f32,
    /// True if audio exceeds noise gate threshold
    pub gate_open: bool,
    /// Instrument presence classifier result
    pub instrument_presence: InstrumentPresence,
}

/// Configuration knobs for onset detection and stem separation.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct DspConfig {
    pub kick_threshold_multiplier: f32,
    pub snare_threshold_multiplier: f32,
    pub min_kick_energy: f32,
    pub min_snare_energy: f32,
    pub decay_alpha: f32,
    pub auto_normalize: bool,
    /// dBFS cutoff threshold (-65.0 to -10.0 dB). Below this, sync is gated off.
    pub noise_gate_db: f32,
    /// User sensitivity scalar (0.2 to 3.0, default 1.0).
    pub sensitivity: f32,
    /// Stem focus mode: FullMix, VocalsOnly, or InstrumentsOnly.
    pub focus_mode: AudioFocusMode,
}

impl Default for DspConfig {
    fn default() -> Self {
        Self {
            kick_threshold_multiplier: 1.38,
            snare_threshold_multiplier: 1.44,
            min_kick_energy: 2.8,
            min_snare_energy: 5.5,
            decay_alpha: 0.055,
            auto_normalize: true,
            noise_gate_db: -45.0,
            sensitivity: 1.0,
            focus_mode: AudioFocusMode::FullMix,
        }
    }
}

/// Fast median of 7 floats without heap allocation.
#[inline]
fn median7(mut arr: [f32; 7]) -> f32 {
    for i in 1..7 {
        let key = arr[i];
        let mut j = i;
        while j > 0 && arr[j - 1] > key {
            arr[j] = arr[j - 1];
            j -= 1;
        }
        arr[j] = key;
    }
    arr[3]
}

/// Real-time FFT spectral flux + Online 2D HPSS + SuperFlux Rhythm & Beat Detector.
pub struct BeatDetector {
    fft: Arc<dyn RealToComplex<f32>>,
    window_size: usize,
    sample_rate: f32,
    scratch: Vec<Complex<f32>>,
    output: Vec<Complex<f32>>,
    windowed: Vec<f32>,
    prev_spectrum: Vec<f32>,
    /// Circular spectrogram history buffer for Online Temporal HPSS (7 frames x spectrum_len)
    spec_history: Vec<Vec<f32>>,
    history_cursor: usize,
    cur_mag: Vec<f32>,
    moving_bass_avg: f32,
    moving_snare_avg: f32,
    moving_inst_avg: f32,
    moving_vocal_avg: f32,
    /// Log-compressed spectral flux state for volume-invariant beat/onset tracking ("SuperFlux")
    prev_log_bass: f32,
    prev_log_snare: f32,
    prev_log_vocal: f32,
    ema_bass_flux: f32,
    ema_snare_flux: f32,
    ema_vocal_flux: f32,
    frames_since_kick: u32,
    frames_since_snare: u32,
    frames_since_vocal: u32,
    rolling_inst_score: f32,
    has_instruments_latched: bool,
    instrument_hold_frames: u32,
    rolling_rms: f32,
    current_gain: f32,
    gate_open: bool,
    gate_hold_frames: u32,
    vocal_hp: Biquad,
    vocal_lp: Biquad,
    inst_notch1: Biquad,
    inst_notch2: Biquad,
    config: DspConfig,
}

impl BeatDetector {
    /// Create a new BeatDetector with specified window size and sample rate.
    pub fn new(window_size: usize, sample_rate: f32, config: DspConfig) -> Self {
        let mut planner = RealFftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(window_size);
        let scratch = fft.make_scratch_vec();
        let output = fft.make_output_vec();
        let spectrum_len = window_size / 2 + 1;

        Self {
            fft,
            window_size,
            sample_rate,
            scratch,
            output,
            windowed: vec![0.0; window_size],
            prev_spectrum: vec![0.0; spectrum_len],
            spec_history: vec![vec![0.0; spectrum_len]; HPSS_TIME_FRAMES],
            history_cursor: 0,
            cur_mag: vec![0.0; spectrum_len],
            moving_bass_avg: 1.0,
            moving_snare_avg: 1.0,
            moving_inst_avg: 1.5,
            moving_vocal_avg: 1.0,
            prev_log_bass: 0.0,
            prev_log_snare: 0.0,
            prev_log_vocal: 0.0,
            ema_bass_flux: 0.06,
            ema_snare_flux: 0.06,
            ema_vocal_flux: 0.05,
            frames_since_kick: 100,
            frames_since_snare: 100,
            frames_since_vocal: 100,
            rolling_inst_score: 0.0,
            has_instruments_latched: false,
            instrument_hold_frames: 0,
            rolling_rms: 0.05,
            current_gain: 1.0,
            gate_open: false,
            gate_hold_frames: 0,
            vocal_hp: Biquad::butterworth_highpass(260.0, sample_rate),
            vocal_lp: Biquad::butterworth_lowpass(3400.0, sample_rate),
            inst_notch1: Biquad::vocal_notch(750.0, sample_rate, 0.9, -12.0),
            inst_notch2: Biquad::vocal_notch(1850.0, sample_rate, 0.9, -12.0),
            config,
        }
    }

    /// Update full DSP configuration.
    pub fn update_config(&mut self, cfg: DspConfig) {
        self.config = cfg;
        if !cfg.auto_normalize {
            self.current_gain = 1.0;
        }
    }

    pub fn set_auto_normalize(&mut self, enabled: bool) {
        self.config.auto_normalize = enabled;
        if !enabled {
            self.current_gain = 1.0;
        }
    }

    pub fn set_noise_gate_db(&mut self, db: f32) {
        self.config.noise_gate_db = db.clamp(-65.0, -10.0);
    }

    pub fn set_sensitivity(&mut self, sens: f32) {
        self.config.sensitivity = sens.clamp(0.2, 3.0);
    }

    pub fn set_focus_mode(&mut self, mode: AudioFocusMode) {
        self.config.focus_mode = mode;
    }

    pub fn current_gain(&self) -> f32 {
        self.current_gain
    }

    pub fn is_gate_open(&self) -> bool {
        self.gate_open
    }

    /// Process a frame of PCM audio samples using Online 2D HPSS + SuperFlux Beat Tracking.
    pub fn process_frame(&mut self, samples: &[f32]) -> Option<BeatEvent> {
        if samples.len() < self.window_size {
            return None;
        }

        // 1. Pre-AGC Raw RMS & Decibel (dBFS) Noise Gate with 3dB Hysteresis
        let sum_sq: f32 = samples[..self.window_size].iter().map(|s| s * s).sum();
        let raw_rms = (sum_sq / self.window_size as f32).sqrt();
        let current_db = 20.0 * raw_rms.max(1e-5).log10();

        if current_db >= self.config.noise_gate_db {
            self.gate_open = true;
            self.gate_hold_frames = 8;
        } else if self.gate_hold_frames > 0 {
            self.gate_hold_frames -= 1;
        } else if current_db < (self.config.noise_gate_db - 3.0) {
            self.gate_open = false;
        }

        if !self.gate_open {
            self.prev_spectrum.fill(0.0);
            for frame in &mut self.spec_history {
                frame.fill(0.0);
            }
            self.prev_log_bass = 0.0;
            self.prev_log_snare = 0.0;
            self.prev_log_vocal = 0.0;
            self.has_instruments_latched = false;
            self.instrument_hold_frames = 0;
            self.rolling_inst_score = 0.0;
            return Some(BeatEvent {
                is_kick: false,
                is_snare: false,
                is_vocal_onset: false,
                bass_intensity: 0.0,
                snare_intensity: 0.0,
                instrument_intensity: 0.0,
                vocal_intensity: 0.0,
                overall_energy: 0.0,
                spectral_flux: 0.0,
                current_db,
                gate_open: false,
                instrument_presence: InstrumentPresence {
                    has_instruments: false,
                    instrument_score: 0.0,
                },
            });
        }

        // 2. Dynamic AGC Normalization
        if self.config.auto_normalize {
            let alpha_rms = 0.025f32;
            self.rolling_rms = (1.0 - alpha_rms) * self.rolling_rms + alpha_rms * raw_rms;
            let target_rms = 0.12f32;
            self.current_gain = if self.rolling_rms > 1e-4 {
                (target_rms / self.rolling_rms).clamp(0.2, 4.0)
            } else {
                1.0
            };
        } else {
            self.current_gain = 1.0;
        }

        // 3. Apply Biquad Pre-Filter Cascade + Hann Window
        let n = self.window_size as f32;
        let gain = self.current_gain;
        for (i, &raw) in samples[..self.window_size].iter().enumerate() {
            let filtered = match self.config.focus_mode {
                AudioFocusMode::FullMix => raw,
                AudioFocusMode::VocalsOnly => {
                    let hp = self.vocal_hp.process(raw);
                    self.vocal_lp.process(hp)
                }
                AudioFocusMode::InstrumentsOnly => {
                    let s1 = self.inst_notch1.process(raw);
                    self.inst_notch2.process(s1)
                }
            };
            let hann = 0.5 * (1.0 - (2.0 * std::f32::consts::PI * i as f32 / (n - 1.0)).cos());
            self.windowed[i] = filtered * hann * gain;
        }

        self.fft
            .process_with_scratch(&mut self.windowed, &mut self.output, &mut self.scratch)
            .ok()?;

        // 4. Populate current magnitude spectrum with micro-frequency noise floor subtraction
        let sens = self.config.sensitivity.clamp(0.2, 3.0);
        let bin_noise_floor = 0.08 / sens.sqrt();
        let spec_len = self.output.len();
        let bin_hz = self.sample_rate / n;

        for (i, comp) in self.output.iter().enumerate() {
            let mag = (comp.norm() - bin_noise_floor).max(0.0);
            self.cur_mag[i] = mag;
            self.spec_history[self.history_cursor][i] = mag;
        }
        self.history_cursor = (self.history_cursor + 1) % HPSS_TIME_FRAMES;

        // 5. Online 2D Harmonic-Percussive Source Separation (HPSS) + Formant Crest Analysis
        let mut sub_bass_energy = 0.0f32;
        let mut percussive_mid_energy = 0.0f32;
        let mut harmonic_vocal_energy = 0.0f32;
        let mut high_inst_energy = 0.0f32;
        let mut total_flux = 0.0f32;

        // Track peak and mean inside the vocal formant band (280Hz - 3100Hz) to compute Crest Factor
        let mut vocal_band_max = 0.0f32;
        let mut vocal_band_sum = 0.0f32;
        let mut vocal_band_bins = 0u32;

        for i in 0..spec_len {
            let freq = i as f32 * bin_hz;
            let mag = self.cur_mag[i];

            // Temporal median across last 7 frames (Harmonic sustained ridges)
            let h_med = median7([
                self.spec_history[0][i],
                self.spec_history[1][i],
                self.spec_history[2][i],
                self.spec_history[3][i],
                self.spec_history[4][i],
                self.spec_history[5][i],
                self.spec_history[6][i],
            ]);

            // Spectral median across frequency neighbors [i-3..=i+3] (Percussive vertical transients)
            let p_med = median7([
                self.cur_mag[i.saturating_sub(3)],
                self.cur_mag[i.saturating_sub(2)],
                self.cur_mag[i.saturating_sub(1)],
                mag,
                self.cur_mag[(i + 1).min(spec_len - 1)],
                self.cur_mag[(i + 2).min(spec_len - 1)],
                self.cur_mag[(i + 3).min(spec_len - 1)],
            ]);

            // Soft Wiener masks for Harmonic (sustained tonal) vs Percussive (transient/broadband)
            let h2 = h_med * h_med;
            let p2 = p_med * p_med;
            let denom = h2 + p2 + 1e-6;
            let mask_harmonic = h2 / denom;
            let mask_percussive = p2 / denom;

            let mag_harmonic = mag * mask_harmonic;
            let mag_percussive = mag * mask_percussive;

            // Log-compressed spectral flux ("SuperFlux") with 3-bin neighbor max suppression
            let prev_neighbor_max = self.prev_spectrum[i.saturating_sub(1)]
                .max(self.prev_spectrum[i])
                .max(self.prev_spectrum[(i + 1).min(spec_len - 1)]);
            let log_cur = (1.0 + 8.0 * mag).ln();
            let log_prev = (1.0 + 8.0 * prev_neighbor_max).ln();
            let diff = log_cur - log_prev;
            if diff > 0.0 {
                total_flux += diff;
            }
            self.prev_spectrum[i] = mag;

            // Band routing:
            // A) Sub-bass & Bass foundation (20Hz - 210Hz): Strictly Instrument (Kick, 808, Bass)
            if (20.0..=210.0).contains(&freq) {
                sub_bass_energy += mag;
            }
            // B) Midrange / Formant region (260Hz - 3200Hz): Split via HPSS + Crest Factor
            else if (260.0..=3200.0).contains(&freq) {
                vocal_band_bins += 1;
                vocal_band_sum += mag;
                if mag > vocal_band_max {
                    vocal_band_max = mag;
                }
                harmonic_vocal_energy += mag_harmonic;
                percussive_mid_energy += mag_percussive;
            }
            // C) High Brilliance (3200Hz - 14000Hz): Guitars, Cymbals, Hi-Hats, Synth harmonics
            else if (3200.0..=14000.0).contains(&freq) {
                high_inst_energy += mag;
            }
        }

        // Compute Formant Harmonic Crest Factor (Peak-to-Mean ratio in 260-3200Hz)
        let vocal_band_mean = if vocal_band_bins > 0 {
            vocal_band_sum / vocal_band_bins as f32
        } else {
            0.0
        };
        let vocal_crest = if vocal_band_mean > 1e-4 {
            (vocal_band_max / vocal_band_mean).clamp(1.0, 12.0)
        } else {
            1.0
        };
        let tonal_specificity = ((vocal_crest - 1.6) / 3.8).clamp(0.15, 1.0);

        let raw_inst_energy =
            sub_bass_energy * 1.35 + percussive_mid_energy * 1.45 + high_inst_energy * 1.25;
        let raw_vocal_tonal = harmonic_vocal_energy * tonal_specificity;

        let vocal_purity = if raw_vocal_tonal + raw_inst_energy > 1e-4 {
            (raw_vocal_tonal / (raw_vocal_tonal + 1.15 * raw_inst_energy)).clamp(0.0, 1.0)
        } else {
            0.0
        };

        let (isolated_inst_energy, isolated_vocal_energy) = match self.config.focus_mode {
            AudioFocusMode::VocalsOnly => (0.0, raw_vocal_tonal * (0.45 + 0.55 * vocal_purity)),
            AudioFocusMode::InstrumentsOnly => (raw_inst_energy, 0.0),
            AudioFocusMode::FullMix => {
                let v_clean = raw_vocal_tonal * vocal_purity.powf(0.85);
                let inst_bleed_guard = (1.0 - (vocal_purity - 0.45).max(0.0) * 1.4).clamp(0.15, 1.0);
                let i_clean = raw_inst_energy * inst_bleed_guard;
                (i_clean, v_clean)
            }
        };

        // 6. Volume-Invariant Log-Flux Onset Detection + Refractory Cooldown
        // Prevents continuous false triggers on sustained loud passages while catching every real beat!
        let alpha = self.config.decay_alpha;
        let prev_bass_avg = self.moving_bass_avg.max(0.2);
        let prev_snare_avg = self.moving_snare_avg.max(0.2);
        let prev_vocal_avg = self.moving_vocal_avg.max(0.2);

        self.moving_bass_avg = (1.0 - alpha) * self.moving_bass_avg + alpha * sub_bass_energy;
        self.moving_snare_avg =
            (1.0 - alpha) * self.moving_snare_avg + alpha * percussive_mid_energy;
        self.moving_inst_avg =
            (1.0 - alpha) * self.moving_inst_avg + alpha * isolated_inst_energy.max(0.5);
        self.moving_vocal_avg =
            (1.0 - alpha) * self.moving_vocal_avg + alpha * isolated_vocal_energy.max(0.5);

        // Log-compressed band energy relative to slow moving average
        let log_bass = (1.0 + 14.0 * (sub_bass_energy / prev_bass_avg)).ln();
        let log_snare = (1.0 + 14.0 * (percussive_mid_energy / prev_snare_avg)).ln();
        let log_vocal = (1.0 + 12.0 * (isolated_vocal_energy / prev_vocal_avg)).ln();

        // First-order positive derivative (transient attack flux, 0 during steady loudness!)
        let bass_attack_flux = (log_bass - self.prev_log_bass).max(0.0);
        let snare_attack_flux = (log_snare - self.prev_log_snare).max(0.0);
        let vocal_attack_flux = (log_vocal - self.prev_log_vocal).max(0.0);

        self.prev_log_bass = 0.65 * log_bass + 0.35 * self.prev_log_bass;
        self.prev_log_snare = 0.65 * log_snare + 0.35 * self.prev_log_snare;
        self.prev_log_vocal = 0.70 * log_vocal + 0.30 * self.prev_log_vocal;

        self.ema_bass_flux = 0.88 * self.ema_bass_flux + 0.12 * bass_attack_flux;
        self.ema_snare_flux = 0.88 * self.ema_snare_flux + 0.12 * snare_attack_flux;
        self.ema_vocal_flux = 0.88 * self.ema_vocal_flux + 0.12 * vocal_attack_flux;

        self.frames_since_kick = self.frames_since_kick.saturating_add(1);
        self.frames_since_snare = self.frames_since_snare.saturating_add(1);
        self.frames_since_vocal = self.frames_since_vocal.saturating_add(1);

        // Sensitivity scales threshold gently (dampened exponent 0.38 so 2.6x doesn't over-trigger on noise)
        let sens_scale = sens.powf(0.38);
        let kick_energy_ratio = sub_bass_energy / prev_bass_avg;
        let snare_energy_ratio = percussive_mid_energy / prev_snare_avg;
        let vocal_energy_ratio = isolated_vocal_energy / prev_vocal_avg;

        let is_kick = self.config.focus_mode != AudioFocusMode::VocalsOnly
            && self.frames_since_kick >= 8 // ~130ms minimum refractory window between kicks
            && sub_bass_energy > (self.config.min_kick_energy / sens)
            && kick_energy_ratio > (self.config.kick_threshold_multiplier / sens_scale)
            && bass_attack_flux > (self.ema_bass_flux * 1.35 + 0.09) / sens_scale;

        if is_kick {
            self.frames_since_kick = 0;
        }

        let is_snare = self.config.focus_mode != AudioFocusMode::VocalsOnly
            && self.frames_since_snare >= 7 // ~115ms minimum refractory window between snares
            && percussive_mid_energy > (self.config.min_snare_energy / sens)
            && snare_energy_ratio > (self.config.snare_threshold_multiplier / sens_scale)
            && snare_attack_flux > (self.ema_snare_flux * 1.40 + 0.10) / sens_scale
            && vocal_purity < 0.45;

        if is_snare {
            self.frames_since_snare = 0;
        }

        let is_vocal_onset = self.config.focus_mode != AudioFocusMode::InstrumentsOnly
            && self.frames_since_vocal >= 10
            && isolated_vocal_energy > (3.2 / sens)
            && (vocal_energy_ratio > (1.26 / sens_scale)
                || vocal_attack_flux > (self.ema_vocal_flux * 1.30 + 0.08) / sens_scale)
            && vocal_purity >= 0.42;

        if is_vocal_onset {
            self.frames_since_vocal = 0;
        }

        // Normalized [0.0 .. 1.0] telemetry intensities
        let bass_intensity =
            (sub_bass_energy / (self.moving_bass_avg * 2.0 + 1e-4)).clamp(0.0, 1.0);
        let snare_intensity =
            (percussive_mid_energy / (self.moving_snare_avg * 2.0 + 1e-4)).clamp(0.0, 1.0);
        let instrument_intensity =
            (isolated_inst_energy / (self.moving_inst_avg * 1.9 + 1e-4)).clamp(0.0, 1.0);
        let vocal_intensity =
            (isolated_vocal_energy / (self.moving_vocal_avg * 1.7 + 1e-4)).clamp(0.0, 1.0);

        // 7. Asymmetric Hysteresis State Machine for Instrument vs. Vocal-Only Classifier
        let raw_inst_score = if isolated_inst_energy + isolated_vocal_energy > 1e-4 {
            (isolated_inst_energy / (isolated_inst_energy + isolated_vocal_energy)).clamp(0.0, 1.0)
        } else {
            0.0
        };

        let alpha_inst = if raw_inst_score > self.rolling_inst_score {
            0.50
        } else {
            0.14
        };
        self.rolling_inst_score =
            (1.0 - alpha_inst) * self.rolling_inst_score + alpha_inst * raw_inst_score;

        let has_instruments = match self.config.focus_mode {
            AudioFocusMode::VocalsOnly => false,
            AudioFocusMode::InstrumentsOnly => true,
            AudioFocusMode::FullMix => {
                let vocal_dominant =
                    vocal_purity > 0.65 && vocal_intensity > 0.40 && instrument_intensity < 0.25;
                if vocal_dominant && !is_kick && !is_snare {
                    self.has_instruments_latched = false;
                    self.instrument_hold_frames = 0;
                    self.rolling_inst_score = (self.rolling_inst_score * 0.4).min(0.20);
                } else if is_kick || is_snare || self.rolling_inst_score >= 0.42 {
                    self.has_instruments_latched = true;
                    self.instrument_hold_frames = 22; // ~330ms hold window
                } else if self.instrument_hold_frames > 0 {
                    self.instrument_hold_frames -= 1;
                } else if self.rolling_inst_score < 0.28 {
                    self.has_instruments_latched = false;
                }
                self.has_instruments_latched
            }
        };

        // Sound level envelope combining dB loudness above noise gate + transient flux
        let db_norm = ((current_db - self.config.noise_gate_db) / 32.0).clamp(0.0, 1.0);
        let flux_norm = ((total_flux * sens_scale) / 95.0).clamp(0.0, 1.0);
        let overall_energy = (db_norm * 0.55 + flux_norm * 0.45).clamp(0.0, 1.0);

        Some(BeatEvent {
            is_kick,
            is_snare,
            is_vocal_onset,
            bass_intensity,
            snare_intensity,
            instrument_intensity,
            vocal_intensity,
            overall_energy,
            spectral_flux: total_flux,
            current_db,
            gate_open: true,
            instrument_presence: InstrumentPresence {
                has_instruments,
                instrument_score: self.rolling_inst_score,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn beat_detector_detects_synthesized_kick_as_instrument_not_vocal() {
        let sample_rate = 44100.0;
        let window_size = 512;
        let mut detector = BeatDetector::new(window_size, sample_rate, DspConfig::default());

        let silence = vec![0.01f32; window_size];
        for _ in 0..10 {
            detector.process_frame(&silence);
        }

        // Generate strong 60Hz kick drum + broadband transient burst
        let mut kick = Vec::with_capacity(window_size);
        for i in 0..window_size {
            let t = i as f32 / sample_rate;
            let sample = (2.0 * std::f32::consts::PI * 60.0 * t).sin() * 0.9;
            kick.push(sample);
        }

        let event = detector.process_frame(&kick).expect("process frame");
        assert!(event.is_kick, "60Hz bass burst should trigger kick detection");
        assert!(
            event.instrument_intensity > 0.45,
            "Kick burst must produce high instrument_intensity, got {}",
            event.instrument_intensity
        );
        assert!(
            event.vocal_intensity < 0.15,
            "Kick burst must NOT trigger vocal_intensity, got {}",
            event.vocal_intensity
        );
        assert!(event.instrument_presence.has_instruments);
    }

    #[test]
    fn sustained_harmonic_vocal_drives_vocal_intensity_without_false_instrument_trigger() {
        let sample_rate = 44100.0;
        let window_size = 512;
        let mut detector = BeatDetector::new(window_size, sample_rate, DspConfig::default());

        let quiet = vec![0.01f32; window_size];
        for _ in 0..8 {
            detector.process_frame(&quiet);
        }

        let mut vocal_frame = Vec::with_capacity(window_size);
        for i in 0..window_size {
            let t = i as f32 / sample_rate;
            let f0 = (2.0 * std::f32::consts::PI * 440.0 * t).sin() * 0.45;
            let f1 = (2.0 * std::f32::consts::PI * 880.0 * t).sin() * 0.30;
            let f2 = (2.0 * std::f32::consts::PI * 1320.0 * t).sin() * 0.15;
            vocal_frame.push(f0 + f1 + f2);
        }

        let mut last_event = None;
        for _ in 0..7 {
            last_event = detector.process_frame(&vocal_frame);
        }

        let ev = last_event.expect("vocal event");
        assert!(
            ev.vocal_intensity > 0.45,
            "Sustained vocal harmonics must produce high vocal_intensity, got {}",
            ev.vocal_intensity
        );
        assert!(
            ev.instrument_intensity < 0.25,
            "Pure sustained vocal harmonics must keep instrument_intensity low, got {}",
            ev.instrument_intensity
        );
        assert!(
            !ev.instrument_presence.has_instruments,
            "Pure vocal should classify has_instruments as false"
        );
    }

    #[test]
    fn agc_normalizes_sustained_loud_signals() {
        let sample_rate = 44100.0;
        let window_size = 512;
        let mut config = DspConfig::default();
        config.auto_normalize = true;
        let mut detector = BeatDetector::new(window_size, sample_rate, config);

        let loud_frame = vec![0.95f32; window_size];
        for _ in 0..50 {
            detector.process_frame(&loud_frame);
        }

        assert!(
            detector.current_gain() < 0.6,
            "Loud audio should compress AGC gain, got {}",
            detector.current_gain()
        );
    }

    #[test]
    fn noise_gate_closes_on_quiet_signals_and_opens_on_speech() {
        let sample_rate = 44100.0;
        let window_size = 512;
        let mut config = DspConfig::default();
        config.noise_gate_db = -40.0;
        let mut detector = BeatDetector::new(window_size, sample_rate, config);

        let quiet = vec![0.001f32; window_size];
        let event_quiet = detector.process_frame(&quiet).expect("quiet frame");
        assert!(!event_quiet.gate_open);

        let active = vec![0.1f32; window_size];
        let event_active = detector.process_frame(&active).expect("active frame");
        assert!(event_active.gate_open);
    }
}
