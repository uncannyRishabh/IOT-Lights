//! `beat-sync`: Real-time music beat-synchronization DSP engine, lighting choreography, and bulb fleet controller.

pub mod audio_in;
pub mod choreographer;
pub mod dsp;
pub mod fleet;

pub use audio_in::{AudioDeviceInfo, AudioStream};
pub use choreographer::{BulbAmbienceMode, ChoreographedCommand, Choreographer, ColorPalette};
pub use dsp::{BeatDetector, BeatEvent, DspConfig};
pub use fleet::{BulbConfig, BulbFleet, FleetBulbInfo};
