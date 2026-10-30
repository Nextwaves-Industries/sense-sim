//! Modbus/TCP server exposing the simulator state as registers, so OpenEMS
//! (or any PLC/SCADA) can poll it with a stock Modbus component.
//!
//! Register map (holding = input registers, unit id ignored):
//! | addr | value |
//! |---|---|
//! | 0 | sim time, seconds (u16, wraps) |
//! | 1 | tags total |
//! | 2 | tags located |
//! | 3 | tags lost |
//! | 4 | infrastructure online (anchors/nodes/readers) |
//! | 5 | infrastructure total |
//! | 6 | active alarms |
//! | 7 | RMSE, cm |
//! | 8 | CEP95, cm |
//! | 9 | RFID reads per second |
//! | 10 | running (1) / paused (0) |
//! | 11 | speed x10 |
//! | 20..59 | zone occupancy, zones sorted by name |
//! | 100 + 10*k | tag k (sorted by id): x mm (i32, hi/lo), y mm (i32), z mm (i32), quality %, age 0.1 s, flags (bit0 lost, bit1 restricted zone) |
//! | 1000 (write) | 1 = run, 0 = pause |
//! | 1001 (write) | speed x10 |
//! | 1002 (write) | 1 = reset metrics |

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossbeam_channel::{Receiver, RecvTimeoutError};

use super::{Control, ModbusCfg, Outgoing, Shared};
use crate::rtls::events::{RtlsEvent, Snapshot};

pub const REGS: usize = 1100;
pub const MAX_TAGS: usize = 89;

pub fn image(s: &Snapshot) -> Vec<u16> {
    let mut r = vec![0u16; REGS];
    r[0] = (s.ts as u64 % 65536) as u16;
    r[1] = s.tags_total as u16;
    r[2] = s.tags_located as u16;
    r[3] = s.tags_lost as u16;
    r[4] = s.infra_online as u16;
    r[5] = s.infra_total as u16;
    r[6] = s.alarms_active as u16;
    r[7] = (s.rmse_m * 100.0).round().clamp(0.0, 65535.0) as u16;
    r[8] = (s.cep95_m * 100.0).round().clamp(0.0, 65535.0) as u16;
    r[9] = s.rfid_reads_per_s.round().clamp(0.0, 65535.0) as u16;
    r[10] = s.running as u16;
    r[11] = (s.speed * 10.0).round().clamp(0.0, 65535.0) as u16;
    for (i, (_, n)) in s.zones.iter().take(40).enumerate() {
        r[20 + i] = *n as u16;
    }
    let mut tags = s.tags.clone();
    tags.sort_by(|a, b| a.id.cmp(&b.id));
    for (k, t) in tags.iter().take(MAX_TAGS).enumerate() {
        let b = 100 + 10 * k;
        for (j, v) in [t.x, t.y, t.z].iter().enumerate() {
            let mm = (v * 1000.0).round() as i32 as u32;
            r[b + 2 * j] = (mm >> 16) as u16;
            r[b + 2 * j + 1] = mm as u16;
        }
        r[b + 6] = (t.quality * 100.0).round().clamp(0.0, 100.0) as u16;
        r[b + 7] = (t.age_s * 10.0).round().clamp(0.0, 65535.0) as u16;
        r[b + 8] = t.lost as u16 | (t.restricted as u16) << 1;
    }
    r
}

/// Handles one request ADU; returns the response ADU.
pub fn handle(req: &[u8], regs: &Mutex<Vec<u16>>, on_write: &dyn Fn(u16, u16)) -> Option<Vec<u8>> {
    if req.len() < 8 {
        return None;
    }
    let (tid, unit, fc) = ([req[0], req[1]], req[6], req[7]);
    let pdu = &req[8..];
    let be = |i: usize| -> Option<u16> { Some(u16::from_be_bytes([*pdu.get(i)?, *pdu.get(i + 1)?])) };
    let exception = |code: u8| vec![fc | 0x80, code];
    let body: Vec<u8> = match fc {
        3 | 4 => {
            let (Some(start), Some(qty)) = (be(0), be(2)) else { return None };
            let (start, qty) = (start as usize, qty as usize);
            if qty == 0 || qty > 125 {
                exception(3)
            } else if start + qty > REGS {
                exception(2)
            } else {
                let r = regs.lock().unwrap();
                let mut b = vec![fc, (qty * 2) as u8];
                for v in &r[start..start + qty] {
                    b.extend_from_slice(&v.to_be_bytes());
                }
                b
            }
        }
        6 => {
            let (Some(addr), Some(val)) = (be(0), be(2)) else { return None };
            if addr as usize >= REGS {
                exception(2)
            } else {
                regs.lock().unwrap()[addr as usize] = val;
                on_write(addr, val);
                pdu[..4].iter().copied().fold(vec![fc], |mut b, x| {
                    b.push(x);
                    b
                })
            }
        }
        16 => {
            let (Some(start), Some(qty)) = (be(0), be(2)) else { return None };
            let n = *pdu.get(4)? as usize;
            if qty as usize * 2 != n || pdu.len() < 5 + n || start as usize + qty as usize > REGS {
                exception(2)
            } else {
                for k in 0..qty as usize {
                    let v = u16::from_be_bytes([pdu[5 + 2 * k], pdu[6 + 2 * k]]);
                    regs.lock().unwrap()[start as usize + k] = v;
                    on_write(start + k as u16, v);
                }
                let mut b = vec![fc];
                b.extend_from_slice(&start.to_be_bytes());
                b.extend_from_slice(&qty.to_be_bytes());
                b
            }
        }
        _ => exception(1),
    };
    let mut out = Vec::with_capacity(7 + body.len());
    out.extend_from_slice(&tid);
    out.extend_from_slice(&[0, 0]);
    out.extend_from_slice(&((body.len() + 1) as u16).to_be_bytes());
    out.push(unit);
    out.extend(body);
    Some(out)
}

fn serve(mut s: TcpStream, regs: Arc<Mutex<Vec<u16>>>, shared: Shared, stop: Arc<AtomicBool>) {
    let _ = s.set_read_timeout(Some(Duration::from_millis(200)));
    let mut buf = Vec::new();
    let mut tmp = [0u8; 512];
    let on_write = |addr: u16, v: u16| match addr {
        1000 => shared.control(Control::Run(v != 0)),
        1001 => shared.control(Control::Speed(v as f32 / 10.0)),
        1002 if v != 0 => shared.control(Control::ResetMetrics),
        _ => {}
    };
    while !stop.load(Ordering::Relaxed) {
        match s.read(&mut tmp) {
            Ok(0) => return,
            Ok(n) => buf.extend_from_slice(&tmp[..n]),
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => continue,
            Err(_) => return,
        }
        while buf.len() >= 7 {
            let len = u16::from_be_bytes([buf[4], buf[5]]) as usize;
            if buf.len() < 6 + len {
                break;
            }
            let adu: Vec<u8> = buf.drain(..6 + len).collect();
            if let Some(resp) = handle(&adu, &regs, &on_write) {
                if s.write_all(&resp).is_err() {
                    return;
                }
                shared.sent(1);
            }
        }
    }
}

pub fn run(cfg: ModbusCfg, rx: Receiver<Outgoing>, shared: Shared) {
    // A previous server on the same port may still be shutting down.
    let mut attempt = 0;
    let listener = loop {
        match TcpListener::bind(cfg.bind.trim()) {
            Ok(l) => break l,
            Err(_) if attempt < 15 => {
                attempt += 1;
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(e) => return shared.set_error(format!("cannot listen on {}: {e}", cfg.bind)),
        }
    };
    let _ = listener.set_nonblocking(true);
    shared.set_connected(format!("listening on {}", cfg.bind));
    let regs = Arc::new(Mutex::new(vec![0u16; REGS]));
    let stop = Arc::new(AtomicBool::new(false));
    {
        let (regs, shared, stop) = (regs.clone(), shared.clone(), stop.clone());
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((s, peer)) => {
                        let _ = s.set_nonblocking(false);
                        shared.set_connected(format!("client {peer}"));
                        let (regs, shared, stop) = (regs.clone(), shared.clone(), stop.clone());
                        std::thread::spawn(move || serve(s, regs, shared, stop));
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(100)),
                }
            }
        });
    }
    loop {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(m) => {
                if let RtlsEvent::Snapshot(s) = &m.event {
                    let mut img = image(s);
                    let mut r = regs.lock().unwrap();
                    // Keep whatever clients wrote to the control block.
                    img[1000..REGS].copy_from_slice(&r[1000..REGS]);
                    *r = img;
                }
            }
            Err(RecvTimeoutError::Disconnected) => {
                stop.store(true, Ordering::Relaxed);
                return;
            }
            Err(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rtls::events::TagRow;

    #[test]
    fn read_holding_registers() {
        let mut s = Snapshot { tags_total: 5, rmse_m: 0.12, ..Default::default() };
        s.tags.push(TagRow { id: "T1".into(), x: -1.5, y: 1.0, z: 2.25, lost: true, ..Default::default() });
        let regs = Mutex::new(image(&s));
        // tid=0x0102, unit 1, FC3, start 0, qty 8
        let req = [1, 2, 0, 0, 0, 6, 1, 3, 0, 0, 0, 8];
        let resp = handle(&req, &regs, &|_, _| {}).unwrap();
        assert_eq!(&resp[..2], &[1, 2]);
        assert_eq!(resp[7], 3);
        assert_eq!(resp[8], 16);
        let reg = |i: usize| u16::from_be_bytes([resp[9 + 2 * i], resp[10 + 2 * i]]);
        assert_eq!(reg(1), 5);
        assert_eq!(reg(7), 12);
        // Tag block: x = -1500 mm as i32
        let req = [0, 1, 0, 0, 0, 6, 1, 4, 0, 100, 0, 9];
        let resp = handle(&req, &regs, &|_, _| {}).unwrap();
        let reg = |i: usize| u16::from_be_bytes([resp[9 + 2 * i], resp[10 + 2 * i]]);
        let x = ((reg(0) as u32) << 16 | reg(1) as u32) as i32;
        assert_eq!(x, -1500);
        assert_eq!(reg(8) & 1, 1);
    }

    #[test]
    fn write_and_errors() {
        let regs = Mutex::new(vec![0u16; REGS]);
        let seen = Mutex::new(Vec::new());
        let req = [0, 9, 0, 0, 0, 6, 1, 6, 0x03, 0xE8, 0, 1]; // write 1000 = 1
        let resp = handle(&req, &regs, &|a, v| seen.lock().unwrap().push((a, v))).unwrap();
        assert_eq!(&resp[7..], &[6, 0x03, 0xE8, 0, 1]);
        assert_eq!(seen.lock().unwrap()[0], (1000, 1));
        let bad = [0, 9, 0, 0, 0, 6, 1, 3, 0xFF, 0x00, 0, 2];
        assert_eq!(handle(&bad, &regs, &|_, _| {}).unwrap()[7..], [0x83, 2]);
        let unknown = [0, 9, 0, 0, 0, 2, 1, 0x2B];
        assert_eq!(handle(&unknown, &regs, &|_, _| {}).unwrap()[7..], [0xAB, 1]);
    }
}
