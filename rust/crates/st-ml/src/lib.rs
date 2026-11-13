//! The models: Whisper and NLLB (CTranslate2 on CUDA), Silero VAD, UVR voice isolation and WeSpeaker
//! speaker fingerprints (ONNX Runtime, DirectML/CPU).
//!
//! Binaries that link this crate must delay-load cuDNN (see apps/st-cli/build.rs) and call
//! `gpu::setup()` before the first model loads.

pub mod gpu;
pub mod isolate;
pub mod mel;
pub mod nllb;
pub mod speakers;
pub mod vad;
pub mod whisper;
