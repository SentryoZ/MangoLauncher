//! Discord Rich Presence over the local IPC socket.
//!
//! Discord exposes a socket per account: `discord-ipc-0` through `-9`, a named
//! pipe on Windows and a Unix socket elsewhere. Messages are a 32-bit opcode, a
//! 32-bit little-endian length, then a JSON payload.
//!
//! Presence is cosmetic, so every failure here is non-fatal: if Discord is not
//! running the launcher plays normally and simply shows nothing.

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::error::{MangoError, Result};

/// Opcodes defined by the IPC protocol.
const OP_HANDSHAKE: u32 = 0;
const OP_FRAME: u32 = 1;
const OP_CLOSE: u32 = 2;
const OP_PING: u32 = 3;

/// How many sockets Discord may publish, one per running client.
const MAX_SOCKETS: u8 = 10;

/// Largest frame accepted from Discord, so a malformed length cannot allocate
/// without bound.
const MAX_FRAME_BYTES: u32 = 1 << 20;

/// Discord rejects activity text outside this range.
const MIN_TEXT: usize = 2;
const MAX_TEXT: usize = 128;

/// Images shown on the presence card.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Assets {
    pub large_image: Option<String>,
    pub large_text: Option<String>,
    pub small_image: Option<String>,
    pub small_text: Option<String>,
}

/// One presence card.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Activity {
    /// First line, e.g. `Playing Mango Survival`.
    pub details: Option<String>,
    /// Second line, e.g. `Minecraft 1.20.1 · Fabric`.
    pub state: Option<String>,
    /// When the activity began, as milliseconds since the Unix epoch.
    pub started_at: Option<u64>,
    pub assets: Assets,
}

#[derive(Debug, Serialize)]
struct Handshake<'a> {
    v: u32,
    client_id: &'a str,
}

#[derive(Debug, Serialize)]
struct SetActivity<'a> {
    cmd: &'static str,
    args: SetActivityArgs<'a>,
    nonce: String,
}

#[derive(Debug, Serialize)]
struct SetActivityArgs<'a> {
    pid: u32,
    activity: Option<&'a Payload>,
}

#[derive(Debug, Serialize, Default)]
pub(crate) struct Payload {
    #[serde(skip_serializing_if = "Option::is_none")]
    details: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    timestamps: Option<Timestamps>,
    #[serde(skip_serializing_if = "Option::is_none")]
    assets: Option<PayloadAssets>,
}

#[derive(Debug, Serialize)]
struct Timestamps {
    start: u64,
}

#[derive(Debug, Serialize)]
struct PayloadAssets {
    #[serde(skip_serializing_if = "Option::is_none")]
    large_image: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    large_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    small_image: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    small_text: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Frame<'a> {
    #[serde(default)]
    evt: Option<&'a str>,
    #[serde(default, rename = "data")]
    data: Option<FrameData<'a>>,
}

#[derive(Debug, Deserialize)]
struct FrameData<'a> {
    #[serde(default)]
    message: Option<&'a str>,
    #[serde(default)]
    code: Option<i64>,
}

impl FrameData<'_> {
    /// Render the error the way Discord's own docs describe it.
    fn describe(&self) -> String {
        match (self.message, self.code) {
            (Some(message), Some(code)) => format!("{message} (code {code})"),
            (Some(message), None) => message.to_owned(),
            (None, Some(code)) => format!("code {code}"),
            (None, None) => "unknown error".to_owned(),
        }
    }
}

/// Trim text to something Discord accepts, or drop it entirely.
///
/// Discord rejects an activity outright when a field is one character long, so a
/// bad value is worse than a missing one.
fn clean_text(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.chars().count() < MIN_TEXT {
        return None;
    }
    Some(trimmed.chars().take(MAX_TEXT).collect())
}

fn clean_image(key: &str) -> Option<String> {
    let trimmed = key.trim();
    if trimmed.is_empty() { None } else { Some(trimmed.to_owned()) }
}

impl Activity {
    /// Convert to the wire payload, dropping anything Discord would refuse.
    pub(crate) fn to_payload(&self) -> Payload {
        let assets = PayloadAssets {
            large_image: self.assets.large_image.as_deref().and_then(clean_image),
            large_text: self.assets.large_text.as_deref().and_then(clean_text),
            small_image: self.assets.small_image.as_deref().and_then(clean_image),
            small_text: self.assets.small_text.as_deref().and_then(clean_text),
        };

        let has_assets = assets.large_image.is_some()
            || assets.large_text.is_some()
            || assets.small_image.is_some()
            || assets.small_text.is_some();

        Payload {
            details: self.details.as_deref().and_then(clean_text),
            state: self.state.as_deref().and_then(clean_text),
            timestamps: self.started_at.map(|start| Timestamps { start }),
            assets: has_assets.then_some(assets),
        }
    }
}

/// Encode one IPC message.
pub fn encode_frame(opcode: u32, payload: &str) -> Vec<u8> {
    let bytes = payload.as_bytes();
    let mut buffer = Vec::with_capacity(8 + bytes.len());
    buffer.extend_from_slice(&opcode.to_le_bytes());
    buffer.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    buffer.extend_from_slice(bytes);
    buffer
}

/// Read one message, used by the reader thread and by tests.
pub fn decode_frame<R: Read>(reader: &mut R) -> std::io::Result<(u32, Vec<u8>)> {
    let mut header = [0u8; 8];
    reader.read_exact(&mut header)?;

    let opcode = u32::from_le_bytes(header[0..4].try_into().expect("4 bytes"));
    let length = u32::from_le_bytes(header[4..8].try_into().expect("4 bytes"));

    if length > MAX_FRAME_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("frame of {length} bytes is too large"),
        ));
    }

    let mut payload = vec![0u8; length as usize];
    reader.read_exact(&mut payload)?;
    Ok((opcode, payload))
}

/// Milliseconds since the Unix epoch, the unit Discord expects.
pub fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

/// An open connection to a running Discord client.
pub struct DiscordIpc {
    writer: Stream,
    client_id: String,
    /// Last error Discord reported, so a rejected client id is not silent.
    errors: Arc<Mutex<Option<String>>>,
}

impl std::fmt::Debug for DiscordIpc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiscordIpc").field("client_id", &self.client_id).finish_non_exhaustive()
    }
}

/// The connection to a Discord client.
///
/// Kept as an enum rather than a trait object so it can be split: each platform
/// has its own `try_clone`, and the reader needs an independent handle.
#[derive(Debug)]
enum Stream {
    #[cfg(windows)]
    Pipe(std::fs::File),
    #[cfg(unix)]
    Unix(std::os::unix::net::UnixStream),
}

impl Stream {
    fn try_clone(&self) -> std::io::Result<Self> {
        match self {
            #[cfg(windows)]
            Self::Pipe(file) => file.try_clone().map(Self::Pipe),
            #[cfg(unix)]
            Self::Unix(stream) => stream.try_clone().map(Self::Unix),
        }
    }
}

impl Read for Stream {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        match self {
            #[cfg(windows)]
            Self::Pipe(file) => file.read(buffer),
            #[cfg(unix)]
            Self::Unix(stream) => stream.read(buffer),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        match self {
            #[cfg(windows)]
            Self::Pipe(file) => file.write(buffer),
            #[cfg(unix)]
            Self::Unix(stream) => stream.write(buffer),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            #[cfg(windows)]
            Self::Pipe(file) => file.flush(),
            #[cfg(unix)]
            Self::Unix(stream) => stream.flush(),
        }
    }
}

#[cfg(windows)]
fn open_socket(index: u8) -> std::io::Result<Stream> {
    use std::fs::OpenOptions;

    // Discord publishes one named pipe per running client.
    let path = format!(r"\\.\pipe\discord-ipc-{index}");
    let file = OpenOptions::new().read(true).write(true).open(path)?;
    Ok(Stream::Pipe(file))
}

#[cfg(unix)]
fn open_socket(index: u8) -> std::io::Result<Stream> {
    use std::os::unix::net::UnixStream;
    use std::path::PathBuf;

    let mut directories: Vec<PathBuf> = Vec::new();
    if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") {
        directories.push(PathBuf::from(dir));
    }
    if let Some(dir) = std::env::var_os("TMPDIR") {
        directories.push(PathBuf::from(dir));
    }
    directories.push(PathBuf::from("/tmp"));

    let mut last_error = None;
    for dir in directories {
        // Flatpak and Snap put the socket under an app-specific directory.
        for candidate in [
            dir.join(format!("discord-ipc-{index}")),
            dir.join(format!("app/com.discordapp.Discord/discord-ipc-{index}")),
        ] {
            match UnixStream::connect(&candidate) {
                Ok(stream) => return Ok(Stream::Unix(stream)),
                Err(err) => last_error = Some(err),
            }
        }
    }

    Err(last_error.unwrap_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "no Discord socket found")
    }))
}

impl DiscordIpc {
    /// Connect to a running Discord client.
    ///
    /// Sockets are tried in order, because each Discord client publishes the
    /// next free index.
    pub fn connect(client_id: &str) -> Result<Self> {
        let client_id = client_id.trim();
        if client_id.is_empty() {
            return Err(MangoError::Other("Discord presence needs a client id".to_owned()));
        }

        let mut socket: Option<Stream> = None;
        for index in 0..MAX_SOCKETS {
            match open_socket(index) {
                Ok(candidate) => {
                    socket = Some(candidate);
                    break;
                }
                Err(err) => debug!(index, %err, "no Discord socket at this index"),
            }
        }

        let socket =
            socket.ok_or_else(|| MangoError::Other("Discord is not running".to_owned()))?;

        let mut writer = socket.try_clone().map_err(|err| {
            MangoError::Other(format!("could not split the Discord socket: {err}"))
        })?;

        let handshake = serde_json::to_string(&Handshake { v: 1, client_id })?;
        writer
            .write_all(&encode_frame(OP_HANDSHAKE, &handshake))
            .map_err(|err| MangoError::Other(format!("Discord handshake failed: {err}")))?;
        let _ = writer.flush();

        // Discord answers every command, and an unread socket eventually blocks
        // its own writer, so replies are drained in the background. A rejected
        // command arrives here as an `ERROR` frame rather than as a write error.
        let errors: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let reader_errors = Arc::clone(&errors);
        let mut reader = socket;
        std::thread::Builder::new()
            .name("mango-discord-reader".to_owned())
            .spawn(move || {
                loop {
                    match decode_frame(&mut reader) {
                        Ok((OP_CLOSE, _)) => {
                            debug!("Discord closed the presence connection");
                            return;
                        }
                        Ok((OP_PING, _)) => debug!("Discord sent a keep-alive"),
                        Ok((opcode, payload)) => match serde_json::from_slice::<Frame>(&payload) {
                            Ok(frame) => {
                                if frame.evt == Some("ERROR") {
                                    let message = frame
                                        .data
                                        .as_ref()
                                        .map(FrameData::describe)
                                        .unwrap_or_else(|| "unknown error".to_owned());
                                    if let Ok(mut slot) = reader_errors.lock() {
                                        *slot = Some(message);
                                    }
                                } else {
                                    debug!(opcode, ?frame.evt, "Discord replied");
                                }
                            }
                            Err(err) => debug!(%err, "Discord sent a frame that could not be read"),
                        },
                        Err(_) => return,
                    }
                }
            })
            .map_err(|err| MangoError::Other(format!("could not read from Discord: {err}")))?;

        Ok(Self { writer, client_id: client_id.to_owned(), errors })
    }

    /// Take the last error Discord reported, if any.
    ///
    /// Discord rejects a command asynchronously, so the failure surfaces on the
    /// next call rather than from [`set_activity`] itself.
    pub fn take_error(&self) -> Option<String> {
        self.errors.lock().ok().and_then(|mut slot| slot.take())
    }

    pub fn client_id(&self) -> &str {
        &self.client_id
    }

    /// Push an activity. `None` clears the presence card.
    pub fn set_activity(&mut self, activity: Option<&Activity>) -> Result<()> {
        let payload = activity.map(Activity::to_payload);
        let body = SetActivity {
            cmd: "SET_ACTIVITY",
            args: SetActivityArgs { pid: std::process::id(), activity: payload.as_ref() },
            nonce: uuid::Uuid::new_v4().to_string(),
        };

        let json = serde_json::to_string(&body)?;
        self.writer
            .write_all(&encode_frame(OP_FRAME, &json))
            .map_err(|err| MangoError::Other(format!("could not send presence: {err}")))?;
        self.writer
            .flush()
            .map_err(|err| MangoError::Other(format!("could not flush presence: {err}")))?;
        Ok(())
    }

    /// Remove the presence card.
    pub fn clear(&mut self) -> Result<()> {
        self.set_activity(None)
    }
}

/// Build the presence card for the launcher's current state.
///
/// Kept separate from the connection so the wording can be tested without a
/// running Discord.
pub fn activity_for(
    manifest: Option<&crate::manifest::Manifest>,
    server: Option<&crate::manifest::ServerEntry>,
    playing: bool,
    started_at: Option<u64>,
) -> Activity {
    let base = manifest.and_then(|m| m.discord.as_ref());
    let assets = Assets {
        large_image: base.and_then(|d| d.large_image_key.clone()),
        large_text: base.and_then(|d| d.large_image_text.clone()),
        small_image: base.and_then(|d| d.small_image_key.clone()),
        small_text: base.and_then(|d| d.small_image_text.clone()),
    };

    let (details, state) = match (server, playing) {
        (Some(server), true) => {
            (format!("Playing {}", server.name), format!("Minecraft {}", server.minecraft))
        }
        (Some(server), false) => (
            "In the launcher".to_owned(),
            format!("{} · Minecraft {}", server.name, server.minecraft),
        ),
        (None, _) => ("In the launcher".to_owned(), "Choosing a server".to_owned()),
    };

    Activity { details: Some(details), state: Some(state), started_at, assets }
}

/// Environment the presence needs from the caller.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PresenceConfig {
    pub client_id: String,
    pub enabled: bool,
}

impl PresenceConfig {
    /// Read the settings that decide whether presence can run at all.
    ///
    /// Both switches must be on: the player's `enableDiscordRpc` and a manifest
    /// that publishes a `discord.clientId`.
    pub fn from_parts(enabled: bool, manifest: Option<&crate::manifest::Manifest>) -> Self {
        let client_id =
            manifest.and_then(|m| m.discord.as_ref()).map(|d| d.client_id.trim().to_owned());
        Self {
            enabled: enabled && client_id.as_deref().is_some_and(|id| !id.is_empty()),
            client_id: client_id.unwrap_or_default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_is_opcode_then_length_then_payload() {
        let frame = encode_frame(OP_HANDSHAKE, r#"{"v":1}"#);
        assert_eq!(&frame[0..4], &0u32.to_le_bytes(), "opcode 0 is little-endian");
        assert_eq!(&frame[4..8], &7u32.to_le_bytes(), "length counts payload bytes only");
        assert_eq!(&frame[8..], br#"{"v":1}"#);
    }

    #[test]
    fn a_frame_round_trips() {
        let frame = encode_frame(OP_FRAME, "hello");
        let (opcode, payload) = decode_frame(&mut frame.as_slice()).unwrap();
        assert_eq!(opcode, OP_FRAME);
        assert_eq!(payload, b"hello");
    }

    #[test]
    fn an_oversized_frame_is_refused_rather_than_allocated() {
        let mut header = Vec::new();
        header.extend_from_slice(&OP_FRAME.to_le_bytes());
        header.extend_from_slice(&(MAX_FRAME_BYTES + 1).to_le_bytes());

        let error = decode_frame(&mut header.as_slice()).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn a_truncated_frame_is_an_error() {
        let frame = encode_frame(OP_FRAME, "hello");
        let error = decode_frame(&mut &frame[..10]).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn one_character_text_is_dropped_because_discord_refuses_it() {
        let activity = Activity {
            details: Some("x".to_owned()),
            state: Some("ok".to_owned()),
            ..Default::default()
        };
        let payload = activity.to_payload();
        assert!(payload.details.is_none(), "a one-character field is invalid");
        assert_eq!(payload.state.as_deref(), Some("ok"));
    }

    #[test]
    fn text_is_trimmed_and_capped() {
        let long = "a".repeat(200);
        let activity = Activity {
            details: Some("  Playing Mango  ".to_owned()),
            state: Some(long.clone()),
            ..Default::default()
        };
        let payload = activity.to_payload();
        assert_eq!(payload.details.as_deref(), Some("Playing Mango"));
        assert_eq!(payload.state.as_ref().unwrap().chars().count(), MAX_TEXT);
    }

    #[test]
    fn empty_assets_are_omitted_not_sent_blank() {
        let activity = Activity {
            details: Some("In the launcher".to_owned()),
            assets: Assets {
                large_image: Some("  ".to_owned()),
                large_text: Some("".to_owned()),
                ..Default::default()
            },
            ..Default::default()
        };
        let json = serde_json::to_value(activity.to_payload()).unwrap();
        assert!(json.get("assets").is_none(), "blank assets must not be serialised: {json}");
    }

    #[test]
    fn assets_are_sent_when_present() {
        let activity = Activity {
            details: Some("Playing Mango".to_owned()),
            assets: Assets {
                large_image: Some("mango-large".to_owned()),
                large_text: Some("Mango Network".to_owned()),
                small_image: Some("mango-small".to_owned()),
                small_text: None,
            },
            ..Default::default()
        };
        let json = serde_json::to_value(activity.to_payload()).unwrap();
        assert_eq!(json["assets"]["large_image"], "mango-large");
        assert_eq!(json["assets"]["large_text"], "Mango Network");
        assert_eq!(json["assets"]["small_image"], "mango-small");
        assert!(
            json["assets"].get("small_text").is_none(),
            "unset text must be omitted, not null: {json}"
        );
    }

    #[test]
    fn a_timestamp_is_only_sent_when_known() {
        let activity = Activity { details: Some("Playing".to_owned()), ..Default::default() };
        let json = serde_json::to_value(activity.to_payload()).unwrap();
        assert!(json.get("timestamps").is_none());

        let activity = Activity { started_at: Some(1_700_000_000_000), ..activity };
        let json = serde_json::to_value(activity.to_payload()).unwrap();
        assert_eq!(json["timestamps"]["start"], 1_700_000_000_000u64);
    }

    #[test]
    fn now_is_reported_in_milliseconds() {
        let now = now_millis();
        // Sanity band: after 2020 and before 2100, in ms.
        assert!(now > 1_577_836_800_000, "{now}");
        assert!(now < 4_102_444_800_000, "{now}");
    }

    #[test]
    fn set_activity_serialises_as_discord_expects() {
        let activity = Activity {
            details: Some("Playing Mango Survival".to_owned()),
            state: Some("Minecraft 1.20.1".to_owned()),
            started_at: Some(1_700_000_000_000),
            assets: Assets::default(),
        };

        let body = SetActivity {
            cmd: "SET_ACTIVITY",
            args: SetActivityArgs { pid: 4321, activity: Some(&activity.to_payload()) },
            nonce: "abc".to_owned(),
        };

        let json = serde_json::to_value(&body).unwrap();
        assert_eq!(json["cmd"], "SET_ACTIVITY");
        assert_eq!(json["args"]["pid"], 4321);
        assert_eq!(json["args"]["activity"]["details"], "Playing Mango Survival");
        assert_eq!(json["args"]["activity"]["state"], "Minecraft 1.20.1");
        assert_eq!(json["nonce"], "abc");
    }

    #[test]
    fn clearing_sends_a_null_activity() {
        let body = SetActivity {
            cmd: "SET_ACTIVITY",
            args: SetActivityArgs { pid: 1, activity: None },
            nonce: "n".to_owned(),
        };
        let json = serde_json::to_value(&body).unwrap();
        assert!(json["args"]["activity"].is_null());
    }

    #[test]
    fn an_empty_client_id_is_rejected_before_connecting() {
        let error = DiscordIpc::connect("   ").unwrap_err();
        assert!(error.to_string().contains("client id"), "{error}");
    }

    #[test]
    fn a_missing_socket_index_reports_why() {
        // Deterministic: index 200 is past anything Discord publishes, so this
        // exercises the not-found path whether or not Discord is running.
        let error = open_socket(200).unwrap_err();
        assert!(
            matches!(error.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::Other),
            "unexpected kind {:?}: {error}",
            error.kind()
        );
    }

    fn manifest_with_discord(client_id: &str) -> crate::manifest::Manifest {
        serde_json::from_str(&format!(
            r#"{{"schemaVersion":1,"name":"Mango Network",
                "discord":{{"clientId":"{client_id}",
                           "largeImageKey":"mango-large","largeImageText":"Mango Network",
                           "smallImageKey":"mango-small","smallImageText":"mangolauncher"}},
                "servers":[{{"id":"main","name":"Mango Survival","address":"play.example.com",
                            "minecraft":"1.20.1","mainServer":true}}]}}"#
        ))
        .unwrap()
    }

    #[test]
    fn presence_needs_both_the_switch_and_a_manifest_id() {
        let manifest = manifest_with_discord("123456789012345678");

        let on = PresenceConfig::from_parts(true, Some(&manifest));
        assert!(on.enabled);
        assert_eq!(on.client_id, "123456789012345678");

        let switched_off = PresenceConfig::from_parts(false, Some(&manifest));
        assert!(!switched_off.enabled, "the player's switch wins");
    }

    #[test]
    fn a_manifest_without_a_discord_block_disables_presence() {
        let manifest: crate::manifest::Manifest = serde_json::from_str(
            r#"{"schemaVersion":1,"name":"Bare","servers":[
                {"id":"a","name":"A","address":"a.example.com","minecraft":"1.20.1"}]}"#,
        )
        .unwrap();

        let config = PresenceConfig::from_parts(true, Some(&manifest));
        assert!(!config.enabled);
        assert!(config.client_id.is_empty());
    }

    #[test]
    fn a_blank_client_id_is_not_used() {
        let manifest = manifest_with_discord("   ");
        let config = PresenceConfig::from_parts(true, Some(&manifest));
        assert!(!config.enabled, "a blank id would fail the handshake");
    }

    #[test]
    fn the_card_names_the_selected_server_in_the_launcher() {
        let manifest = manifest_with_discord("1");
        let server = manifest.server("main");

        let activity = activity_for(Some(&manifest), server, false, None);
        assert_eq!(activity.details.as_deref(), Some("In the launcher"));
        assert_eq!(activity.state.as_deref(), Some("Mango Survival · Minecraft 1.20.1"));
        assert!(activity.started_at.is_none(), "no timer before the game starts");
    }

    #[test]
    fn the_card_says_playing_once_the_game_runs() {
        let manifest = manifest_with_discord("1");
        let server = manifest.server("main");

        let activity = activity_for(Some(&manifest), server, true, Some(1_700_000_000_000));
        assert_eq!(activity.details.as_deref(), Some("Playing Mango Survival"));
        assert_eq!(activity.state.as_deref(), Some("Minecraft 1.20.1"));
        assert_eq!(activity.started_at, Some(1_700_000_000_000));
    }

    #[test]
    fn the_card_falls_back_when_nothing_is_selected() {
        let manifest = manifest_with_discord("1");

        let activity = activity_for(Some(&manifest), None, false, None);
        assert_eq!(activity.details.as_deref(), Some("In the launcher"));
        assert_eq!(activity.state.as_deref(), Some("Choosing a server"));
    }

    #[test]
    fn images_come_from_the_manifest() {
        let manifest = manifest_with_discord("1");
        let activity = activity_for(Some(&manifest), manifest.server("main"), false, None);

        assert_eq!(activity.assets.large_image.as_deref(), Some("mango-large"));
        assert_eq!(activity.assets.large_text.as_deref(), Some("Mango Network"));
        assert_eq!(activity.assets.small_image.as_deref(), Some("mango-small"));
        assert_eq!(activity.assets.small_text.as_deref(), Some("mangolauncher"));
    }

    #[test]
    fn an_error_frame_is_turned_into_a_message() {
        let frame: Frame = serde_json::from_str(
            r#"{"cmd":"SET_ACTIVITY","evt":"ERROR","data":{"code":4000,"message":"Invalid Client ID"}}"#,
        )
        .unwrap();

        assert_eq!(frame.evt, Some("ERROR"));
        assert_eq!(
            frame.data.as_ref().map(FrameData::describe).as_deref(),
            Some("Invalid Client ID (code 4000)")
        );
    }

    #[test]
    fn an_error_frame_without_a_code_is_still_readable() {
        let frame: Frame =
            serde_json::from_str(r#"{"evt":"ERROR","data":{"message":"Something went wrong"}}"#)
                .unwrap();
        assert_eq!(
            frame.data.as_ref().map(FrameData::describe).as_deref(),
            Some("Something went wrong")
        );
    }

    #[test]
    fn a_normal_reply_is_not_mistaken_for_an_error() {
        let frame: Frame =
            serde_json::from_str(r#"{"cmd":"SET_ACTIVITY","evt":null,"data":{"activity":{}}}"#)
                .unwrap();
        assert_ne!(frame.evt, Some("ERROR"));
    }

    #[test]
    fn no_manifest_still_produces_a_valid_card() {
        let activity = activity_for(None, None, false, None);
        assert_eq!(activity.details.as_deref(), Some("In the launcher"));
        assert!(activity.assets.large_image.is_none());

        // And it must survive the wire conversion, which drops short text.
        let payload = activity.to_payload();
        assert_eq!(payload.details.as_deref(), Some("In the launcher"));
    }
}
