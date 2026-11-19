//! Discord voice: a bot joins your team's voice channel and hears each person as a separate stream,
//! named after their Discord account. Works in end-to-end encrypted (DAVE) calls (songbird 0.6).
//!
//! The bot only listens; it needs the View Channel and Connect permissions and no privileged intents.
//! Your token stays on this PC (settings.json) and is only sent to Discord.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use serenity::{
    all::{ChannelType, Http},
    async_trait,
    client::{Client, Context, EventHandler},
    model::{
        gateway::Ready,
        id::{ChannelId, GuildId, UserId},
    },
    prelude::GatewayIntents,
};
use songbird::{
    driver::{DecodeConfig, DecodeMode},
    model::payload::Speaking,
    Config, CoreEvent, Event, EventContext, EventHandler as VoiceEventHandler, SerenityInit,
};
use st_core::cancel::Cancel;

use crate::devices::Resampler;

/// View Channel + Connect: all the bot needs.
const PERMISSIONS: u64 = 1024 | 1048576;
/// 20 ms of 48 kHz audio
const TICK: usize = 960;

#[derive(Clone, Debug)]
pub struct DiscordTarget {
    pub token: String,
    pub guild: u64,
    pub channel: u64,
}

/// What the call reports back. `audio(user, 16 kHz mono)` arrives every 20 ms for everyone in the call
/// (silence included, so sentences end on time); `name(user, display name)` once per person;
/// `problem(message, fatal)` when something goes wrong.
pub struct Callbacks {
    pub audio: Box<dyn Fn(u64, &[f32]) + Send + Sync>,
    /// (user, display name, small avatar URL)
    pub name: Box<dyn Fn(u64, &str, Option<&str>) + Send + Sync>,
    pub problem: Box<dyn Fn(&str, bool) + Send + Sync>,
    pub joined: Box<dyn Fn(&str) + Send + Sync>,
}

fn runtime() -> Result<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build()?)
}

fn token_error(e: impl std::fmt::Display) -> anyhow::Error {
    let s = e.to_string();
    if s.contains("401") || s.to_lowercase().contains("unauthorized") || s.contains("Invalid authentication") {
        anyhow!("Discord didn't accept the bot token. Copy it again from the Developer Portal (Bot → Reset Token).")
    } else {
        anyhow!("Couldn't reach Discord: {s}")
    }
}

/// The bot's name, an invite link for it, and the voice channels it can see, grouped by server.
pub fn channels(token: &str) -> Result<Value> {
    let token = token.trim().to_string();
    if token.is_empty() {
        return Err(anyhow!("Paste the bot token first."));
    }
    runtime()?.block_on(async move {
        let http = Http::new(&token);
        let me = http.get_current_user().await.map_err(token_error)?;
        let invite = format!("https://discord.com/oauth2/authorize?client_id={}&scope=bot&permissions={PERMISSIONS}", me.id);
        let mut servers = vec![];
        for g in http.get_guilds(None, None).await.map_err(token_error)? {
            let mut list: Vec<_> = g
                .id
                .channels(&http)
                .await
                .map(|m| m.into_values().filter(|c| matches!(c.kind, ChannelType::Voice | ChannelType::Stage)).collect())
                .unwrap_or_else(|_| vec![]);
            list.sort_by_key(|c| c.position);
            servers.push(json!({"id": g.id.to_string(), "name": g.name,
                "channels": list.iter().map(|c| json!({"id": c.id.to_string(), "name": c.name})).collect::<Vec<_>>()}));
        }
        Ok(json!({"bot": me.name, "invite": invite, "servers": servers}))
    })
}

/// Per-stream state in the receiver.
struct Stream {
    user: Option<u64>,
    rs: Resampler,
    /// audio that arrived before Discord said whose stream it is
    early: Vec<f32>,
}

#[derive(Clone)]
struct Receiver {
    streams: Arc<Mutex<HashMap<u32, Stream>>>,
    cb: Arc<Callbacks>,
    named: Arc<Mutex<HashMap<u64, String>>>,
    http: Arc<Http>,
    guild: GuildId,
}

impl Receiver {
    fn feed(&self, ssrc: u32, pcm48: &[f32]) {
        let mut streams = self.streams.lock().unwrap();
        let st = streams.entry(ssrc).or_insert_with(|| Stream { user: None, rs: Resampler::new(48000.0), early: vec![] });
        let mut out = vec![];
        st.rs.process(pcm48, &mut out);
        match st.user {
            Some(u) => (self.cb.audio)(u, &out),
            None => {
                st.early.extend_from_slice(&out);
                let cap = 16000 * 5;
                if st.early.len() > cap {
                    let extra = st.early.len() - cap;
                    st.early.drain(..extra);
                }
            }
        }
    }

    fn learn_name(&self, user: u64) {
        if self.named.lock().unwrap().contains_key(&user) {
            return;
        }
        self.named.lock().unwrap().insert(user, String::new());
        let me = self.clone();
        tokio::spawn(async move {
            let (name, face) = match me.guild.member(&me.http, UserId::new(user)).await {
                Ok(m) => (m.display_name().to_string(), Some(m.face())),
                Err(_) => match me.http.get_user(UserId::new(user)).await {
                    Ok(u) => (u.global_name.clone().unwrap_or(u.name.clone()), Some(u.face())),
                    Err(_) => (format!("User {}", user % 10000), None),
                },
            };
            // a small one is plenty next to a name
            let face = face.map(|f| match f.split_once('?') {
                Some((base, _)) => format!("{base}?size=64"),
                None => format!("{f}?size=64"),
            });
            me.named.lock().unwrap().insert(user, name.clone());
            (me.cb.name)(user, &name, face.as_deref());
        });
    }
}

#[async_trait]
impl VoiceEventHandler for Receiver {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> {
        match ctx {
            EventContext::SpeakingStateUpdate(Speaking { ssrc, user_id: Some(u), .. }) => {
                self.learn_name(u.0);
                let early = {
                    let mut streams = self.streams.lock().unwrap();
                    let st = streams.entry(*ssrc).or_insert_with(|| Stream { user: None, rs: Resampler::new(48000.0), early: vec![] });
                    st.user = Some(u.0);
                    std::mem::take(&mut st.early)
                };
                if !early.is_empty() {
                    (self.cb.audio)(u.0, &early);
                }
            }
            EventContext::VoiceTick(tick) => {
                let mut mono = vec![0f32; TICK];
                for (ssrc, data) in &tick.speaking {
                    match &data.decoded_voice {
                        Some(v) if v.len() >= 2 => {
                            mono.clear();
                            mono.extend(v.chunks_exact(2).map(|p| (p[0] as f32 + p[1] as f32) / 65536.0));
                        }
                        _ => mono.iter_mut().for_each(|s| *s = 0.0),
                    }
                    self.feed(*ssrc, &mono);
                }
                // people who are in the call but quiet: silence keeps their timeline going
                let quiet = vec![0f32; TICK];
                for ssrc in &tick.silent {
                    self.feed(*ssrc, &quiet);
                }
            }
            EventContext::DriverDisconnect(_) => (self.cb.problem)("Lost the connection to the voice channel.", true),
            _ => {}
        }
        None
    }
}

struct Handler {
    target: DiscordTarget,
    cb: Arc<Callbacks>,
}

#[async_trait]
impl EventHandler for Handler {
    async fn ready(&self, ctx: Context, _ready: Ready) {
        let guild = GuildId::new(self.target.guild);
        let Some(manager) = songbird::get(&ctx).await else {
            (self.cb.problem)("Discord voice didn't start.", true);
            return;
        };
        let rx = Receiver { streams: Default::default(), cb: self.cb.clone(), named: Default::default(), http: ctx.http.clone(), guild };
        {
            let call = manager.get_or_insert(guild);
            let mut call = call.lock().await;
            call.add_global_event(CoreEvent::SpeakingStateUpdate.into(), rx.clone());
            call.add_global_event(CoreEvent::VoiceTick.into(), rx.clone());
            call.add_global_event(CoreEvent::DriverDisconnect.into(), rx.clone());
        }
        // Still in the call from last time (the app was closed suddenly)? Discord doesn't answer a join to the
        // channel it thinks the bot is already in, so leave first, then join; retry a couple of times.
        let channel = ChannelId::new(self.target.channel);
        let mut last_err = String::new();
        for attempt in 0..3 {
            if let Some(call) = manager.get(guild) {
                let _ = call.lock().await.leave().await;
            }
            tokio::time::sleep(Duration::from_millis(if attempt == 0 { 300 } else { 1500 })).await;
            match tokio::time::timeout(Duration::from_secs(12), manager.join(guild, channel)).await {
                Ok(Ok(_)) => {
                    let name = channel.name(&ctx.http).await.unwrap_or_else(|_| "the voice channel".into());
                    (self.cb.joined)(&name);
                    return;
                }
                Ok(Err(e)) => last_err = e.to_string(),
                Err(_) => last_err = "Discord didn't answer".into(),
            }
        }
        (self.cb.problem)(
            &format!("The bot couldn't join the voice channel ({last_err}). Check that it's in the server and has the Connect permission there."),
            true,
        );
    }
}

/// A running bot in the call. Dropping it (or `stop`) leaves the channel and logs out.
pub struct DiscordCall {
    stop: Cancel,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl DiscordCall {
    pub fn start(target: DiscordTarget, cb: Callbacks) -> Result<DiscordCall> {
        let rt = runtime()?;
        let stop = Cancel::new();
        let s2 = stop.clone();
        let cb = Arc::new(cb);
        let thread = std::thread::spawn(move || {
            rt.block_on(async move {
                let guild = GuildId::new(target.guild);
                let intents = GatewayIntents::GUILDS | GatewayIntents::GUILD_VOICE_STATES;
                let config = Config::default().decode_mode(DecodeMode::Decode(DecodeConfig::default()));
                let built = Client::builder(target.token.trim(), intents)
                    .event_handler(Handler { target: target.clone(), cb: cb.clone() })
                    .register_songbird_from_config(config)
                    .await;
                let mut client = match built {
                    Ok(c) => c,
                    Err(e) => return (cb.problem)(&token_error(e).to_string(), true),
                };
                let shards = client.shard_manager.clone();
                let voice = client.data.read().await.get::<songbird::SongbirdKey>().cloned();
                let watch = async {
                    while !s2.is_set() {
                        tokio::time::sleep(Duration::from_millis(200)).await;
                    }
                };
                tokio::select! {
                    r = client.start() => if let Err(e) = r { if !s2.is_set() { (cb.problem)(&token_error(e).to_string(), true) } },
                    _ = watch => {}
                }
                if let Some(v) = voice {
                    let _ = v.remove(guild).await;
                    // let the "leave" reach Discord before the connection closes, or the bot lingers in the call
                    tokio::time::sleep(Duration::from_millis(600)).await;
                }
                shards.shutdown_all().await;
            });
            rt.shutdown_timeout(Duration::from_secs(2));
        });
        Ok(DiscordCall { stop, thread: Some(thread) })
    }

    pub fn stop(&mut self) {
        self.stop.set();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for DiscordCall {
    fn drop(&mut self) {
        self.stop();
    }
}
