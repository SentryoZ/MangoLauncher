//! Minecraft Server List Ping (modern protocol).
//!
//! The legacy `0xFE 0x01` ping cannot report the player count on modern
//! servers, so the launcher speaks the current handshake/status exchange over a
//! plain TCP socket.

use std::time::{Duration, Instant};

use serde::Deserialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::error::{MangoError, Result};

/// Protocol version sent in the handshake.
///
/// `-1` asks the server to reply regardless of its version, which is what we
/// want for a status display.
const STATUS_PROTOCOL: i32 = -1;
/// Guard against a hostile or broken server sending an enormous frame.
const MAX_FRAME: usize = 2 * 1024 * 1024;

/// What the launcher shows under a server's name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerStatus {
    pub version_name: String,
    pub protocol: i32,
    pub players_online: u32,
    pub players_max: u32,
    /// MOTD with legacy colour codes stripped.
    pub motd: String,
    pub favicon: Option<String>,
    pub latency_ms: u64,
}

#[derive(Debug, Deserialize)]
struct StatusResponse {
    version: VersionInfo,
    players: PlayersInfo,
    #[serde(default)]
    description: Description,
    #[serde(default)]
    favicon: Option<String>,
}

#[derive(Debug, Deserialize)]
struct VersionInfo {
    #[serde(default)]
    name: String,
    #[serde(default)]
    protocol: i32,
}

#[derive(Debug, Deserialize)]
struct PlayersInfo {
    #[serde(default)]
    online: u32,
    #[serde(default)]
    max: u32,
}

#[derive(Debug, Default, Deserialize)]
#[serde(untagged)]
enum Description {
    #[default]
    Missing,
    Text(String),
    Chat(ChatText),
}

#[derive(Debug, Deserialize)]
struct ChatText {
    #[serde(default)]
    text: String,
    #[serde(default)]
    extra: Vec<ChatText>,
}

impl Description {
    fn flatten(&self) -> String {
        match self {
            Description::Missing => String::new(),
            Description::Text(text) => text.clone(),
            Description::Chat(chat) => {
                let mut out = chat.text.clone();
                for part in &chat.extra {
                    out.push_str(&part.text);
                    for nested in &part.extra {
                        out.push_str(&nested.text);
                    }
                }
                out
            }
        }
    }
}

/// Ping `host[:port]`, defaulting to port 25565.
pub async fn ping(address: &str, timeout: Duration) -> Result<ServerStatus> {
    let (host, port) = split_address(address);
    let started = Instant::now();

    let fut = async {
        let mut stream = TcpStream::connect((host.as_str(), port)).await?;
        stream.set_nodelay(true).ok();

        // Handshake: protocol, host, port, next state = status.
        let mut handshake = Vec::new();
        write_varint(&mut handshake, 0x00);
        write_varint(&mut handshake, STATUS_PROTOCOL);
        write_string(&mut handshake, &host);
        handshake.extend_from_slice(&port.to_be_bytes());
        write_varint(&mut handshake, 1);
        write_frame(&mut stream, &handshake).await?;

        // Status request: an empty packet with id 0x00.
        let mut request = Vec::new();
        write_varint(&mut request, 0x00);
        write_frame(&mut stream, &request).await?;

        let frame = read_frame(&mut stream).await?;
        let mut cursor = &frame[..];
        let packet_id = read_varint(&mut cursor)?;
        if packet_id != 0x00 {
            return Err(MangoError::Other(format!("unexpected status packet id {packet_id}")));
        }
        let json = read_string(&mut cursor)?;

        let parsed: StatusResponse = serde_json::from_str(&json)?;
        Ok::<StatusResponse, MangoError>(parsed)
    };

    let response = tokio::time::timeout(timeout, fut)
        .await
        .map_err(|_| MangoError::Other(format!("{address} did not answer within {timeout:?}")))??;

    Ok(ServerStatus {
        version_name: response.version.name,
        protocol: response.version.protocol,
        players_online: response.players.online,
        players_max: response.players.max,
        motd: strip_formatting(&response.description.flatten()),
        favicon: response.favicon,
        latency_ms: started.elapsed().as_millis() as u64,
    })
}

/// Split `host:port`, tolerating bare hosts and IPv6 in brackets.
pub fn split_address(address: &str) -> (String, u16) {
    let trimmed = address.trim();

    if let Some(rest) = trimmed.strip_prefix('[')
        && let Some((host, tail)) = rest.split_once(']')
    {
        let port = tail.strip_prefix(':').and_then(|p| p.parse().ok()).unwrap_or(25565);
        return (host.to_owned(), port);
    }

    // Require a single colon so bare IPv6 addresses are not mangled.
    if trimmed.matches(':').count() == 1
        && let Some((host, port)) = trimmed.rsplit_once(':')
        && let Ok(port) = port.parse::<u16>()
    {
        return (host.to_owned(), port);
    }

    (trimmed.to_owned(), 25565)
}

/// Remove `§x` colour/format codes so the MOTD renders as plain text.
pub fn strip_formatting(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '§' {
            chars.next();
        } else {
            out.push(c);
        }
    }
    out
}

fn write_varint(out: &mut Vec<u8>, value: i32) {
    let mut value = value as u32;
    loop {
        let mut byte = (value & 0x7F) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if value == 0 {
            break;
        }
    }
}

fn read_varint(cursor: &mut &[u8]) -> Result<i32> {
    let mut result: u32 = 0;
    for shift in 0..5 {
        let Some((&byte, rest)) = cursor.split_first() else {
            return Err(MangoError::Other("truncated varint".to_owned()));
        };
        *cursor = rest;
        result |= ((byte & 0x7F) as u32) << (7 * shift);
        if byte & 0x80 == 0 {
            return Ok(result as i32);
        }
    }
    Err(MangoError::Other("varint is too long".to_owned()))
}

fn write_string(out: &mut Vec<u8>, text: &str) {
    write_varint(out, text.len() as i32);
    out.extend_from_slice(text.as_bytes());
}

fn read_string(cursor: &mut &[u8]) -> Result<String> {
    let len = read_varint(cursor)?;
    if len < 0 || len as usize > MAX_FRAME {
        return Err(MangoError::Other(format!("invalid string length {len}")));
    }
    let len = len as usize;
    if cursor.len() < len {
        return Err(MangoError::Other("truncated string".to_owned()));
    }
    let (bytes, rest) = cursor.split_at(len);
    *cursor = rest;
    String::from_utf8(bytes.to_vec())
        .map_err(|err| MangoError::Other(format!("invalid utf-8 in status: {err}")))
}

async fn write_frame(stream: &mut TcpStream, payload: &[u8]) -> Result<()> {
    let mut frame = Vec::with_capacity(payload.len() + 5);
    write_varint(&mut frame, payload.len() as i32);
    frame.extend_from_slice(payload);
    stream.write_all(&frame).await?;
    stream.flush().await?;
    Ok(())
}

async fn read_frame(stream: &mut TcpStream) -> Result<Vec<u8>> {
    let length = read_varint_stream(stream).await?;
    if length < 0 || length as usize > MAX_FRAME {
        return Err(MangoError::Other(format!("invalid frame length {length}")));
    }

    let mut buffer = vec![0u8; length as usize];
    stream.read_exact(&mut buffer).await?;
    Ok(buffer)
}

async fn read_varint_stream(stream: &mut TcpStream) -> Result<i32> {
    let mut result: u32 = 0;
    for shift in 0..5 {
        let mut byte = [0u8; 1];
        stream.read_exact(&mut byte).await?;
        result |= ((byte[0] & 0x7F) as u32) << (7 * shift);
        if byte[0] & 0x80 == 0 {
            return Ok(result as i32);
        }
    }
    Err(MangoError::Other("varint is too long".to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varint_round_trips() {
        for value in [0, 1, 127, 128, 255, 300, 25565, 1_000_000] {
            let mut buffer = Vec::new();
            write_varint(&mut buffer, value);
            let mut cursor = &buffer[..];
            assert_eq!(read_varint(&mut cursor).unwrap(), value);
            assert!(cursor.is_empty());
        }
    }

    #[test]
    fn negative_protocol_version_round_trips() {
        let mut buffer = Vec::new();
        write_varint(&mut buffer, STATUS_PROTOCOL);
        let mut cursor = &buffer[..];
        assert_eq!(read_varint(&mut cursor).unwrap(), STATUS_PROTOCOL);
    }

    #[test]
    fn varint_rejects_overlong_encoding() {
        let buffer = [0x80u8; 6];
        let mut cursor = &buffer[..];
        assert!(read_varint(&mut cursor).is_err());
    }

    #[test]
    fn strings_round_trip_with_length_prefix() {
        let mut buffer = Vec::new();
        write_string(&mut buffer, "mc.example.com");
        let mut cursor = &buffer[..];
        assert_eq!(read_string(&mut cursor).unwrap(), "mc.example.com");
    }

    #[test]
    fn address_splitting_handles_common_forms() {
        assert_eq!(split_address("play.example.com:25565"), ("play.example.com".to_owned(), 25565));
        assert_eq!(split_address("play.example.com"), ("play.example.com".to_owned(), 25565));
        assert_eq!(split_address("play.example.com:1337"), ("play.example.com".to_owned(), 1337));
        assert_eq!(split_address("[::1]:25566"), ("::1".to_owned(), 25566));
        // Bare IPv6 must not be mistaken for host:port.
        assert_eq!(split_address("::1"), ("::1".to_owned(), 25565));
        assert_eq!(split_address("  host:25570  "), ("host".to_owned(), 25570));
    }

    #[test]
    fn formatting_codes_are_stripped() {
        assert_eq!(strip_formatting("§aHello §lWorld§r"), "Hello World");
        assert_eq!(strip_formatting("plain"), "plain");
        assert_eq!(strip_formatting("trailing§"), "trailing");
    }

    #[test]
    fn chat_description_is_flattened() {
        let json = r#"{"text":"A","extra":[{"text":"B"},{"text":"C","extra":[{"text":"D"}]}]}"#;
        let description: Description = serde_json::from_str(json).unwrap();
        assert_eq!(description.flatten(), "ABCD");
    }

    #[test]
    fn plain_string_description_is_supported() {
        let description: Description = serde_json::from_str("\"Hello server\"").unwrap();
        assert_eq!(description.flatten(), "Hello server");
    }

    #[test]
    fn full_status_response_parses() {
        let json = r#"{
            "version": {"name": "Paper 1.20.1", "protocol": 763},
            "players": {"max": 20, "online": 5},
            "description": {"text": "§aWelcome"},
            "favicon": "data:image/png;base64,AAA"
        }"#;
        let parsed: StatusResponse = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.version.name, "Paper 1.20.1");
        assert_eq!(parsed.players.online, 5);
        assert_eq!(strip_formatting(&parsed.description.flatten()), "Welcome");
        assert!(parsed.favicon.is_some());
    }
}
