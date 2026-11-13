//! StreamTranslator core: pure logic shared by the CLI and the app (no GPU, no network).

pub mod cancel;
pub mod chat;
pub mod cluster;
pub mod dsp;
pub mod glossary;
pub mod langs;
pub mod models;
pub mod ocrtext;
pub mod paths;
pub mod py;
pub mod subs;
pub mod sync;
pub mod timefmt;
pub mod vod;

/// A job error with a message meant for the user.
#[derive(Debug)]
pub struct JobError(pub String);

impl std::fmt::Display for JobError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for JobError {}

/// The user cancelled.
#[derive(Debug)]
pub struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("Cancelled")
    }
}
impl std::error::Error for Cancelled {}

/// `Err(JobError(msg))` as an anyhow error.
pub fn job_err<T>(msg: impl Into<String>) -> anyhow::Result<T> {
    Err(JobError(msg.into()).into())
}
