//! DSP Beat and Transient Onset Detection using Real-to-Complex FFT and Spectral Flux.

use std::sync::Arc;
use num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};
use serde::{Deserialize, Serialize};

/// Detected beat and musical transient event.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BeatEvent {
    /// Strong low-frequency kick drum or 808 transient hit
    pub is_kick: bool,
    /// Mid-frequency snare, clap, or vocal punch
    pub is_snare: bool,
    /// Normalized sub-bass energy (0.0 to 1.0)
    pub bass_intensity: f32,
    /// Normalized snare/mid energy (0.0 to 1.0)
    pub snare_intensity: f32,
    /// Overall spectral flux / dynamics change (0.0 to 1.0)
    pub overall_energy: f32,
    /// Raw calculated spectral flux value
    pub spectral_flux: f32,
}

/// Configuration knobs for onset detection.
#[derive(Debug, Clone, Copy)]
pub struct DspConfig {
    pub kick_threshold_multiplier: f32,
    pub snare_threshold_multiplier: f32,
    pub min_kick_energy: f32,
    pub min_snare_energy: f32,
    pub decay_alpha: f32,
    pub auto_normalize: bool,
}

impl Default for DspConfig {
    fn default() -> Self {
        Self {
            kick_threshold_multiplier: 1.45,
            snare_threshold_multiplier: 1.6,
            min_kick_energy: 3.5,
            min_snare_energy: 8.0,
            decay_alpha: 0.06,
            auto_normalize: true,
        }
    }
}

/// Real-time FFT spectral flux beat and onset detector.
pub struct BeatDetector {
    fft: Arc<dyn RealToComplex<f32>>,
    window_size: usize,
    sample_rate: f32,
    scratch: Vec<Complex<f32>>,
    output: Vec<Complex<f32>>,
    windowed: Vec<f32>,
    prev_spectrum: Vec<f32>,
    moving_bass_avg: f32,
    moving_snare_avg: f32,
    rolling_rms: f32,
    current_gain: f32,
    config: DspConfig,
}

impl BeatDetector {
    /// Create a new BeatDetector with specified window size (e.g. 512 or 1024) and sample rate (e.g. 44100.0 or 48000.0).
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
            moving_bass_avg: 1.0,
            moving_snare_avg: 1.0,
            rolling_rms: 0.1,
            current_gain: 1.0,
            config,
        }
    }

    /// Set auto-normalization AGC state.
    pub fn set_auto_normalize(&mut self, enabled: bool) {
        self.config.auto_normalize = enabled;
        if !enabled {
            self.current_gain = 1.0;
        }
    }

    /// Query current AGC dynamic gain multiplier.
    pub fn current_gain(&self) -> f32 {
        self.current_gain
    }

    /// Process a frame of PCM audio samples (length must be >= window_size).
    /// Allocation-free hot path using pre-allocated FFT and window buffers.
    pub fn process_frame(&mut self, samples: &[f32]) -> Option<BeatEvent> {
        if samples.len() < self.window_size {
            return None;
        }

        // Dynamic Volume Normalization (AGC): Calculate frame RMS and track rolling envelope
        if self.config.auto_normalize {
            let sum_sq: f32 = samples[..self.window_size].iter().map(|s| s * s).sum();
            let frame_rms = (sum_sq / self.window_size as f32).sqrt();

            // Smooth envelope (~1-2s effective time constant at ~60Hz)
            let alpha_rms = 0.025f32;
            self.rolling_rms = (1.0 - alpha_rms) * self.rolling_rms + alpha_rms * frame_rms;

            // Target nominal RMS: 0.12, clamped between 0.2x and 4.0x gain
            let target_rms = 0.12f32;
            if self.rolling_rms > 1e-4 {
                let raw_gain = target_rms / self.rolling_rms;
                self.current_gain = raw_gain.clamp(0.2, 4.0);
            } else {
                self.current_gain = 1.0;
            }
        } else {
            self.current_gain = 1.0;
        }

        let gain = self.current_gain;
        self.windowed.copy_from_slice(&samples[..self.window_size]);

        // Apply Hann window and AGC gain to reduce spectral leakage & normalize
        let n = self.window_size as f32;
        for (i, sample) in self.windowed.iter_mut().enumerate() {
            let multiplier = 0.5 * (1.0 - (2.0 * std::f32::consts::PI * i as f32 / (n - 1.0)).cos());
            *sample = *sample * multiplier * gain;
        }

        self.fft
            .process_with_scratch(&mut self.windowed, &mut self.output, &mut self.scratch)
            .ok()?;

        let bin_hz = self.sample_rate / (self.window_size as f32);
        let mut bass_energy = 0.0f32;
        let mut snare_energy = 0.0f32;
        let mut total_flux = 0.0f32;

        for (i, comp) in self.output.iter().enumerate() {
            let freq = i as f32 * bin_hz;
            let mag = comp.norm();

            // Half-wave rectified spectral flux
            let prev = self.prev_spectrum[i];
            let diff = mag - prev;
            if diff > 0.0 {
                total_flux += diff;
            }
            self.prev_spectrum[i] = mag;

            // Sub-bass & Bass: 20Hz - 150Hz
            if (20.0..=150.0).contains(&freq) {
                bass_energy += mag;
            }
            // Snares, claps & prominent mid transients: 250Hz - 2500Hz
            else if (250.0..=2500.0).contains(&freq) {
                snare_energy += mag;
            }
        }

        // Exponential moving average for dynamic thresholding
        let alpha = self.config.decay_alpha;
        self.moving_bass_avg = (1.0 - alpha) * self.moving_bass_avg + alpha * bass_energy;
        self.moving_snare_avg = (1.0 - alpha) * self.moving_snare_avg + alpha * snare_energy;

        let kick_threshold = self.moving_bass_avg * self.config.kick_threshold_multiplier;
        let is_kick = bass_energy > kick_threshold && bass_energy > self.config.min_kick_energy;

        let snare_threshold = self.moving_snare_avg * self.config.snare_threshold_multiplier;
        let is_snare = snare_energy > snare_threshold && snare_energy > self.config.min_snare_energy;

        let bass_intensity = (bass_energy / (self.moving_bass_avg * 2.0 + 1e-4)).clamp(0.0, 1.0);
        let snare_intensity = (snare_energy / (self.moving_snare_avg * 2.0 + 1e-4)).clamp(0.0, 1.0);
        let overall_energy = (total_flux / 400.0).clamp(0.0, 1.0);

        Some(BeatEvent {
            is_kick,
            is_snare,
            bass_intensity,
            snare_intensity,
            overall_energy,
            spectral_flux: total_flux,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn beat_detector_detects_synthesized_kick() {
        let sample_rate = 44100.0;
        let window_size = 512;
        let mut detector = BeatDetector::new(window_size, sample_rate, DspConfig::default());

        // Prime moving average with quiet noise
        let silence = vec![0.01f32; window_size];
        for _ in 0..10 {
            detector.process_frame(&silence);
        }

        // Generate strong 60Hz kick drum burst
        let mut kick = Vec::with_capacity(window_size);
        for i in 0..window_size {
            let t = i as f32 / sample_rate;
            let sample = (2.0 * std::f32::consts::PI * 60.0 * t).sin() * 0.9;
            kick.push(sample);
        }

        let event = detector.process_frame(&kick).expect("process frame");
        assert!(event.is_kick, "60Hz bass burst should trigger kick detection");
        assert!(event.bass_intensity > 0.4);
    }

    #[test]
    fn agc_normalizes_sustained_loud_signals() {
        let sample_rate = 44100.0;
        let window_size = 512;
        let mut config = DspConfig::default();
        config.auto_normalize = true;
        let mut detector = BeatDetector::new(window_size, sample_rate, config);

        // Feed extremely loud 0.95 amplitude signal
        let loud_frame = vec![0.95f32; window_size];
        for _ in 0..50 {
            detector.process_frame(&loud_frame);
        }

        // AGC gain should have compressed downward to avoid pegging
        assert!(detector.current_gain() < 0.6, "Loud audio should compress AGC gain, got {}", detector.current_gain());
    }
}
