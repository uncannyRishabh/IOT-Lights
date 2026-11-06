//! `beat-sync`: Real-time music beat-synchronization DSP engine, IMAX prominent color engine, and bulb fleet controller.

pub mod audio_in;
pub mod choreographer;
pub mod dsp;
pub mod fleet;
pub mod imax;

pub use audio_in::{AudioDeviceInfo, AudioStream};
pub use choreographer::{
    BulbAmbienceMode, ChoreographedCommand, Choreographer, ColorPalette, TransitionMode,
};
pub use dsp::{AudioFocusMode, BeatDetector, BeatEvent, DspConfig, InstrumentPresence};
pub use fleet::{BulbConfig, BulbFleet, FleetBulbInfo};
pub use imax::{DisplayInfo, ImaxColorMode, ImaxConfig, ImaxEngine};
