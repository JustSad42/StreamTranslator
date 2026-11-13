//! Live mode: stream audio to captions (session), Twitch chat translation (chat) and the OBS browser
//! source (obs). The floating caption window is part of the app.

pub mod chat;
pub mod obs;
pub mod session;

pub use session::{Emit, LiveOptions, LiveSession};
