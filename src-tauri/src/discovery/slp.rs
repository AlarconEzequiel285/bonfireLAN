//! Server List Ping, 1.7+ protocol.

use std::net::SocketAddr;
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;

use crate::contracts::{DiscoveredServer, Players};

/// -1 makes the server report its own version.
const PROTOCOL_VERSION: i32 = -1;
const CONNECT_TIMEOUT: Duration = Duration::from_millis(800);
const IO_TIMEOUT: Duration = Duration::from_millis(1500);

pub async fn ping(addr: SocketAddr) -> Option<DiscoveredServer> {
    let result = timeout(IO_TIMEOUT, ping_inner(addr)).await.ok()??;
    Some(result)
}

async fn ping_inner(addr: SocketAddr) -> Option<DiscoveredServer> {
    let mut stream = timeout(CONNECT_TIMEOUT, TcpStream::connect(addr))
        .await
        .ok()?
        .ok()?;

    let host = addr.ip().to_string();
    let port = addr.port();

    let mut handshake = Vec::new();
    write_varint(&mut handshake, 0x00); // packet id
    write_varint(&mut handshake, PROTOCOL_VERSION);
    write_string(&mut handshake, &host);
    handshake.extend_from_slice(&port.to_be_bytes());
    write_varint(&mut handshake, 1); // next state = status
    write_framed(&mut stream, &handshake).await.ok()?;

    let mut request = Vec::new();
    write_varint(&mut request, 0x00); // packet id
    write_framed(&mut stream, &request).await.ok()?;

    let _packet_len = read_varint(&mut stream).await.ok()?;
    let _packet_id = read_varint(&mut stream).await.ok()?;
    let json_len = read_varint(&mut stream).await.ok()?;
    if json_len <= 0 || json_len > 1_048_576 {
        return None;
    }
    let mut buf = vec![0u8; json_len as usize];
    stream.read_exact(&mut buf).await.ok()?;
    let json: Value = serde_json::from_slice(&buf).ok()?;

    Some(build_server(addr, &json))
}

fn build_server(addr: SocketAddr, json: &Value) -> DiscoveredServer {
    let name = json
        .get("description")
        .map(extract_text)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| addr.ip().to_string());

    let mc_version = json
        .pointer("/version/name")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();

    let online = json
        .pointer("/players/online")
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;
    let max = json
        .pointer("/players/max")
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;

    // SLP can't distinguish vanilla from Fabric (they look identical). Forge
    // advertises itself via `forgeData`/`modinfo`. Everything else stays
    // "unknown" until the host manifest confirms the loader.
    let loader = if json.get("forgeData").is_some() || json.get("modinfo").is_some() {
        "forge"
    } else {
        "unknown"
    }
    .to_string();

    let favicon_base64 = json
        .get("favicon")
        .and_then(Value::as_str)
        .map(|s| s.to_string());

    DiscoveredServer {
        id: format!("{}:{}", addr.ip(), addr.port()),
        ip: addr.ip().to_string(),
        port: addr.port(),
        name,
        mc_version,
        loader,
        players: Players { online, max },
        favicon_base64,
        bonfirelan_ready: false,
        manifest_url: None,
    }
}

/// Chat component (string | {text, extra}) to plain text.
fn extract_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Object(map) => {
            let mut out = String::new();
            if let Some(Value::String(t)) = map.get("text") {
                out.push_str(t);
            }
            if let Some(Value::Array(extra)) = map.get("extra") {
                for e in extra {
                    out.push_str(&extract_text(e));
                }
            }
            out
        }
        _ => String::new(),
    }
}

async fn write_framed(stream: &mut TcpStream, payload: &[u8]) -> std::io::Result<()> {
    let mut frame = Vec::with_capacity(payload.len() + 5);
    write_varint(&mut frame, payload.len() as i32);
    frame.extend_from_slice(payload);
    stream.write_all(&frame).await
}

fn write_varint(buf: &mut Vec<u8>, value: i32) {
    let mut val = value as u32;
    loop {
        let mut byte = (val & 0x7f) as u8;
        val >>= 7;
        if val != 0 {
            byte |= 0x80;
        }
        buf.push(byte);
        if val == 0 {
            break;
        }
    }
}

fn write_string(buf: &mut Vec<u8>, s: &str) {
    write_varint(buf, s.len() as i32);
    buf.extend_from_slice(s.as_bytes());
}

async fn read_varint(stream: &mut TcpStream) -> std::io::Result<i32> {
    let mut result: i32 = 0;
    let mut shift = 0;
    loop {
        let byte = stream.read_u8().await?;
        result |= ((byte & 0x7f) as i32) << shift;
        if byte & 0x80 == 0 {
            break;
        }
        shift += 7;
        if shift >= 35 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "VarInt too long",
            ));
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::{extract_text, write_varint};
    use serde_json::json;

    #[test]
    fn varint_encodes_known_values() {
        let mut buf = Vec::new();
        write_varint(&mut buf, 0);
        assert_eq!(buf, vec![0x00]);

        buf.clear();
        write_varint(&mut buf, 127);
        assert_eq!(buf, vec![0x7f]);

        buf.clear();
        write_varint(&mut buf, 128);
        assert_eq!(buf, vec![0x80, 0x01]);

        buf.clear();
        write_varint(&mut buf, 300);
        assert_eq!(buf, vec![0xac, 0x02]);
    }

    #[test]
    fn extracts_plain_string_description() {
        assert_eq!(extract_text(&json!("Hello")), "Hello");
    }

    #[test]
    fn extracts_chat_component_description() {
        let v = json!({ "text": "A", "extra": [{ "text": "B" }, "C"] });
        assert_eq!(extract_text(&v), "ABC");
    }
}
