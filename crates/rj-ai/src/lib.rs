//! AI subtitles (SPEC §7): audio → whisper transcript → optional Opus-MT
//! translation → cleaned-up SRT.
//!
//! Each stage is a trait so the pipeline and job queue work (and are tested)
//! without real models.

pub mod pipeline;
pub mod spm;
pub mod srt;
#[cfg(feature = "translate")]
pub mod translate;
pub mod whisper_cli;

/// One timed line of text. Times in milliseconds.
#[derive(Debug, Clone, PartialEq)]
pub struct Cue {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("audio extraction failed: {0}")]
    Audio(String),
    #[error("missing {0}")]
    ModelMissing(String),
    #[error("transcription failed: {0}")]
    Transcribe(String),
    #[error("translation failed: {0}")]
    Translate(String),
    #[error("cancelled")]
    Cancelled,
}
