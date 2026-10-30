//! Minimal MQTT 3.1.1 client on std::net (QoS 0/1 publish, subscribe,
//! keep-alive, auto-reconnect). Enough for OpenEMS / any broker without
//! pulling an async runtime into the app.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError};

use super::{Feedback, MqttCfg, Outgoing, SinkStatus, Shared};

pub fn encode_len(mut n: usize, out: &mut Vec<u8>) {
    loop {
        let mut b = (n % 128) as u8;
        n /= 128;
        if n > 0 {
            b |= 0x80;
        }
        out.push(b);
        if n == 0 {
            break;
        }
    }
}

fn put_str(s: &str, out: &mut Vec<u8>) {
    out.extend_from_slice(&(s.len() as u16).to_be_bytes());
    out.extend_from_slice(s.as_bytes());
}

fn packet(header: u8, body: &[u8]) -> Vec<u8> {
    let mut out = vec![header];
    encode_len(body.len(), &mut out);
    out.extend_from_slice(body);
    out
}

pub fn connect_packet(client_id: &str, user: &str, pass: &str, keep_alive: u16) -> Vec<u8> {
    let mut b = Vec::new();
    put_str("MQTT", &mut b);
    b.push(4); // protocol level 3.1.1
    let mut flags = 0x02; // clean session
    if !user.is_empty() {
        flags |= 0x80;
        if !pass.is_empty() {
            flags |= 0x40;
        }
    }
    b.push(flags);
    b.extend_from_slice(&keep_alive.to_be_bytes());
    put_str(client_id, &mut b);
    if !user.is_empty() {
        put_str(user, &mut b);
        if !pass.is_empty() {
            put_str(pass, &mut b);
        }
    }
    packet(0x10, &b)
}

pub fn publish_packet(topic: &str, payload: &[u8], qos: u8, retain: bool, packet_id: u16) -> Vec<u8> {
    let mut b = Vec::new();
    put_str(topic, &mut b);
    if qos > 0 {
        b.extend_from_slice(&packet_id.to_be_bytes());
    }
    b.extend_from_slice(payload);
    packet(0x30 | (qos.min(1) << 1) | retain as u8, &b)
}

pub fn subscribe_packet(packet_id: u16, filter: &str, qos: u8) -> Vec<u8> {
    let mut b = packet_id.to_be_bytes().to_vec();
    put_str(filter, &mut b);
    b.push(qos.min(1));
    packet(0x82, &b)
}

/// One complete packet from the front of `buf`: (first byte, body, total length).
pub fn parse_frame(buf: &[u8]) -> Option<(u8, &[u8], usize)> {
    if buf.len() < 2 {
        return None;
    }
    let mut len = 0usize;
    let mut mult = 1usize;
    let mut i = 1;
    loop {
        let b = *buf.get(i)?;
        len += (b & 0x7f) as usize * mult;
        mult *= 128;
        i += 1;
        if b & 0x80 == 0 {
            break;
        }
        if i > 4 {
            return None;
        }
    }
    let end = i + len;
    (buf.len() >= end).then(|| (buf[0], &buf[i..end], end))
}

/// Topic and payload of an incoming PUBLISH body, plus its packet id for QoS 1.
pub fn parse_publish(first: u8, body: &[u8]) -> Option<(String, Vec<u8>, Option<u16>)> {
    let tl = u16::from_be_bytes([*body.first()?, *body.get(1)?]) as usize;
    let topic = String::from_utf8_lossy(body.get(2..2 + tl)?).to_string();
    let mut p = 2 + tl;
    let qos = (first >> 1) & 3;
    let id = if qos > 0 {
        let id = u16::from_be_bytes([*body.get(p)?, *body.get(p + 1)?]);
        p += 2;
        Some(id)
    } else {
        None
    };
    Some((topic, body.get(p..)?.to_vec(), id))
}

pub fn run(cfg: MqttCfg, rx: Receiver<Outgoing>, shared: Shared) {
    let mut backoff = Duration::from_millis(500);
    loop {
        shared.set_state(SinkStatus::connecting(&cfg.host_port()));
        match session(&cfg, &rx, &shared) {
            Ok(()) => return, // channel closed: sink removed
            Err(e) => {
                shared.set_error(format!("{e}"));
                // Keep draining so the app never blocks while the broker is down.
                let until = Instant::now() + backoff;
                while Instant::now() < until {
                    match rx.recv_timeout(Duration::from_millis(100)) {
                        Ok(_) => shared.dropped(1),
                        Err(RecvTimeoutError::Disconnected) => return,
                        Err(_) => {}
                    }
                }
                backoff = (backoff * 2).min(Duration::from_secs(10));
            }
        }
    }
}

fn session(cfg: &MqttCfg, rx: &Receiver<Outgoing>, shared: &Shared) -> anyhow::Result<()> {
    let addr = cfg.host_port().to_socket_addrs()?.next().ok_or_else(|| anyhow::anyhow!("cannot resolve {}", cfg.host_port()))?;
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(3))?;
    s.set_nodelay(true)?;
    s.set_read_timeout(Some(Duration::from_millis(20)))?;
    let keep_alive = 30u16;
    let client_id = if cfg.client_id.is_empty() { format!("sensesim-{}", std::process::id()) } else { cfg.client_id.clone() };
    s.write_all(&connect_packet(&client_id, &cfg.username, &cfg.password, keep_alive))?;

    let mut buf: Vec<u8> = Vec::new();
    let mut tmp = [0u8; 4096];
    let start = Instant::now();
    let mut connected = false;
    let mut next_id: u16 = 1;
    let mut last_tx = Instant::now();
    while !connected {
        if start.elapsed() > Duration::from_secs(5) {
            anyhow::bail!("no CONNACK from broker");
        }
        match s.read(&mut tmp) {
            Ok(0) => anyhow::bail!("broker closed the connection"),
            Ok(n) => buf.extend_from_slice(&tmp[..n]),
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
            Err(e) => return Err(e.into()),
        }
        if let Some((first, body, used)) = parse_frame(&buf) {
            if first >> 4 == 2 {
                let code = body.get(1).copied().unwrap_or(255);
                if code != 0 {
                    anyhow::bail!("broker refused connection (code {code}{})", if code == 4 || code == 5 { ": bad credentials" } else { "" });
                }
                connected = true;
            }
            buf.drain(..used);
        }
    }
    for filter in [cfg.subscribe.trim(), &format!("{}/control", cfg.prefix.trim_end_matches('/'))] {
        if !filter.is_empty() {
            s.write_all(&subscribe_packet(next_id, filter, 0))?;
            next_id = next_id.wrapping_add(1).max(1);
        }
    }
    shared.set_connected(format!("connected to {}", cfg.host_port()));

    loop {
        match rx.recv_timeout(Duration::from_millis(20)) {
            Ok(out) => {
                let mut batch = vec![out];
                batch.extend(rx.try_iter().take(500));
                let mut bytes = Vec::new();
                for o in batch {
                    let topic = format!("{}/{}", cfg.prefix.trim_end_matches('/'), o.topic);
                    bytes.extend(publish_packet(&topic, o.payload.as_bytes(), cfg.qos, cfg.retain && o.retain, next_id));
                    if cfg.qos > 0 {
                        next_id = next_id.wrapping_add(1).max(1);
                    }
                    shared.sent(1);
                }
                s.write_all(&bytes)?;
                last_tx = Instant::now();
            }
            Err(RecvTimeoutError::Disconnected) => {
                let _ = s.write_all(&[0xE0, 0]);
                return Ok(());
            }
            Err(_) => {}
        }
        if last_tx.elapsed() > Duration::from_secs(keep_alive as u64 / 2) {
            s.write_all(&[0xC0, 0])?;
            last_tx = Instant::now();
        }
        match s.read(&mut tmp) {
            Ok(0) => anyhow::bail!("broker closed the connection"),
            Ok(n) => buf.extend_from_slice(&tmp[..n]),
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
            Err(e) => return Err(e.into()),
        }
        while let Some((first, body, used)) = parse_frame(&buf) {
            if first >> 4 == 3
                && let Some((topic, payload, id)) = parse_publish(first, body)
            {
                if let Some(id) = id {
                    s.write_all(&[0x40, 2, (id >> 8) as u8, id as u8])?;
                }
                let text = String::from_utf8_lossy(&payload).to_string();
                shared.feedback(Feedback { source: format!("mqtt:{topic}"), value: text.clone() });
                if topic.ends_with("/control") {
                    shared.control_json(&text);
                }
            }
            buf.drain(..used);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remaining_length_varint() {
        for (n, want) in [(0usize, vec![0u8]), (127, vec![0x7f]), (128, vec![0x80, 1]), (16383, vec![0xff, 0x7f]), (2_097_151, vec![0xff, 0xff, 0x7f])] {
            let mut v = Vec::new();
            encode_len(n, &mut v);
            assert_eq!(v, want, "{n}");
        }
    }

    #[test]
    fn publish_roundtrip() {
        let p = publish_packet("a/b", b"{\"x\":1}", 1, false, 7);
        let (first, body, used) = parse_frame(&p).unwrap();
        assert_eq!(used, p.len());
        let (topic, payload, id) = parse_publish(first, body).unwrap();
        assert_eq!(topic, "a/b");
        assert_eq!(payload, b"{\"x\":1}");
        assert_eq!(id, Some(7));
    }

    #[test]
    fn connect_has_credentials() {
        let p = connect_packet("cid", "u", "p", 30);
        assert_eq!(p[0], 0x10);
        let (_, body, _) = parse_frame(&p).unwrap();
        assert_eq!(&body[2..6], b"MQTT");
        assert_eq!(body[7], 0x02 | 0x80 | 0x40);
    }

    #[test]
    fn partial_frame_waits() {
        let p = publish_packet("t", &[1u8; 300], 0, false, 0);
        assert!(parse_frame(&p[..100]).is_none());
        assert!(parse_frame(&p).is_some());
    }
}
