//! The announcement of the server on the local network (mDNS, `_eh3d._tcp`), so that the programs and the apps can find it without being told its address.
//!
//! It is off by default: the switch `mdns_announce` (`server.cfg` and the security page of Studio, `PUT /api/v1/auth/policy`) turns it on and off while the server runs. While it
//! is on, the server answers the multicast questions that ask for `_eh3d._tcp.local`, for its own instance name or for its host name, with what it knows and nothing else: the
//! port it listens on, its version, the path of the interface and the address of this computer when one can be told (a server that listens on this computer only is not
//! announced at all, and a computer with no address that can be told announces no address). The code is plain `std`: one UDP socket on port 5353, a packet no bigger than the
//! limits below, no more than a few answers per second. When another program owns the port (a system responder such as Avahi), the server does not take it: it says so and tries
//! again later; `docs/DEPLOY.md` has the file for Avahi to publish the same service.

use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::state::AppState;

/// The service type, as labels of a name.
pub const SERVICE: [&str; 3] = ["_eh3d", "_tcp", "local"];
const META: [&str; 4] = ["_services", "_dns-sd", "_udp", "local"];
pub const MDNS_PORT: u16 = 5353;
pub const GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 251);
/// The biggest packet that is read; a bigger one is dropped without being looked at.
pub const MAX_PACKET: usize = 1500;
/// The biggest packet that is sent.
pub const MAX_RESPONSE: usize = 1400;
/// The most questions of a packet that are read; a packet with more is dropped.
pub const MAX_QUESTIONS: usize = 16;
/// How long the records live in the cache of the others, in seconds. The server says them again every `REANNOUNCE`, so a server that is gone is forgotten in two minutes.
pub const TTL: u32 = 120;
const REANNOUNCE: Duration = Duration::from_secs(60);
const RETRY_BIND: Duration = Duration::from_secs(30);
/// The most answers sent in one second, all together.
const MAX_ANSWERS_PER_SECOND: u32 = 20;
const MAX_LABEL: usize = 63;
const MAX_TXT: usize = 200;

const TYPE_A: u16 = 1;
const TYPE_PTR: u16 = 12;
const TYPE_TXT: u16 = 16;
const TYPE_SRV: u16 = 33;
const TYPE_ANY: u16 = 255;

/// One question of a packet: the labels of the name in lower case, the type and whether it asks for a reply to the sender only.
#[derive(Debug, Clone, PartialEq)]
pub struct Question {
    pub name: Vec<String>,
    pub qtype: u16,
    pub unicast: bool,
}

/// What the server announces.
#[derive(Debug, Clone, PartialEq)]
pub struct Announce {
    /// The name of the instance, one label (no dots, 63 bytes at most).
    pub instance: String,
    /// The name of the computer, one label of letters, digits and hyphens.
    pub host: String,
    pub port: u16,
    /// The address of this computer, when it can be told.
    pub addr: Option<Ipv4Addr>,
    /// The `key=value` lines of the TXT record.
    pub txt: Vec<String>,
}

/// A label that is safe to send: no dots or control characters, cut at 63 bytes on a boundary of characters, and `fallback` when nothing is left.
pub fn clean_label(raw: &str, fallback: &str) -> String {
    let mut out = String::new();
    for c in raw.chars() {
        if c == '.' || c.is_control() {
            continue;
        }
        if out.len() + c.len_utf8() > MAX_LABEL {
            break;
        }
        out.push(c);
    }
    let out = out.trim().to_string();
    if out.is_empty() {
        fallback.to_string()
    } else {
        out
    }
}

/// The name of the computer as a label for `.local`: lower case letters, digits and hyphens.
pub fn host_label(raw: &str) -> String {
    let mut out: String = raw
        .chars()
        .map(|c| c.to_ascii_lowercase())
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .take(MAX_LABEL)
        .collect();
    while out.starts_with('-') {
        out.remove(0);
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        "eh3d-server".to_string()
    } else {
        out
    }
}

fn put_name(out: &mut Vec<u8>, labels: &[&str]) {
    for l in labels {
        let bytes = l.as_bytes();
        let n = bytes.len().min(MAX_LABEL);
        out.push(n as u8);
        out.extend_from_slice(&bytes[..n]);
    }
    out.push(0);
}

fn put_record(out: &mut Vec<u8>, labels: &[&str], rtype: u16, flush: bool, ttl: u32, data: &[u8]) {
    put_name(out, labels);
    out.extend_from_slice(&rtype.to_be_bytes());
    let class: u16 = if flush { 0x8001 } else { 1 };
    out.extend_from_slice(&class.to_be_bytes());
    out.extend_from_slice(&ttl.to_be_bytes());
    out.extend_from_slice(&(data.len() as u16).to_be_bytes());
    out.extend_from_slice(data);
}

fn name_bytes(labels: &[&str]) -> Vec<u8> {
    let mut v = Vec::new();
    put_name(&mut v, labels);
    v
}

/// Reads a name at `pos` (following the pointers of compression, a few times at most). Gives the labels in lower case and moves `pos` past the name.
fn read_name(packet: &[u8], pos: &mut usize) -> Option<Vec<String>> {
    let mut labels: Vec<String> = Vec::new();
    let mut at = *pos;
    let mut jumped = false;
    let mut jumps = 0;
    let mut total = 0usize;
    loop {
        let len = *packet.get(at)? as usize;
        if len == 0 {
            if !jumped {
                *pos = at + 1;
            }
            return Some(labels);
        }
        if len & 0xC0 == 0xC0 {
            let low = *packet.get(at + 1)? as usize;
            if !jumped {
                *pos = at + 2;
                jumped = true;
            }
            jumps += 1;
            if jumps > 8 {
                return None;
            }
            at = ((len & 0x3F) << 8) | low;
            continue;
        }
        if len > MAX_LABEL {
            return None;
        }
        let text = packet.get(at + 1..at + 1 + len)?;
        total += len + 1;
        if total > 255 || labels.len() >= 32 {
            return None;
        }
        labels.push(String::from_utf8_lossy(text).to_ascii_lowercase());
        at += 1 + len;
    }
}

/// The questions of a packet that is a query; `None` for anything else (an answer, a packet that is too big, one that does not read, one with too many questions).
pub fn parse_query(packet: &[u8]) -> Option<Vec<Question>> {
    if packet.len() < 12 || packet.len() > MAX_PACKET {
        return None;
    }
    let flags = u16::from_be_bytes([packet[2], packet[3]]);
    // a response, or an opcode that is not a query
    if flags & 0x8000 != 0 || flags & 0x7800 != 0 {
        return None;
    }
    let count = u16::from_be_bytes([packet[4], packet[5]]) as usize;
    if count == 0 || count > MAX_QUESTIONS {
        return None;
    }
    let mut pos = 12;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let name = read_name(packet, &mut pos)?;
        let tail = packet.get(pos..pos + 4)?;
        let qtype = u16::from_be_bytes([tail[0], tail[1]]);
        let class = u16::from_be_bytes([tail[2], tail[3]]);
        pos += 4;
        out.push(Question {
            name,
            qtype,
            unicast: class & 0x8000 != 0,
        });
    }
    Some(out)
}

fn same(name: &[String], labels: &[&str]) -> bool {
    name.len() == labels.len()
        && name
            .iter()
            .zip(labels)
            .all(|(a, b)| a.eq_ignore_ascii_case(b))
}

/// Which records go into a packet.
#[derive(Default, Clone, Copy)]
struct Want {
    meta: bool,
    ptr: bool,
    srv: bool,
    txt: bool,
    a: bool,
}

impl Announce {
    /// The settings and the address the server listens on, made into what is announced. `bound` is the address the server listens on; `local` the address of this computer.
    pub fn new(
        name: &str,
        host: &str,
        bound: SocketAddr,
        local: Option<Ipv4Addr>,
        version: &str,
    ) -> Announce {
        let host = host_label(host);
        let instance = if name.trim().is_empty() {
            clean_label(&format!("EH3D Server {host}"), "EH3D Server")
        } else {
            clean_label(name, "EH3D Server")
        };
        // an address the server was told to listen on is the one to announce; the address of the computer only when it listens on all of them
        let addr = match bound.ip() {
            std::net::IpAddr::V4(a) if !a.is_unspecified() && !a.is_loopback() => Some(a),
            std::net::IpAddr::V4(_) => local,
            std::net::IpAddr::V6(_) => None,
        };
        Announce {
            instance,
            host,
            port: bound.port(),
            addr,
            txt: vec![
                "txtvers=1".to_string(),
                "path=/api/v1".to_string(),
                format!("version={version}"),
            ],
        }
    }

    fn instance_labels(&self) -> [&str; 4] {
        [&self.instance, SERVICE[0], SERVICE[1], SERVICE[2]]
    }

    fn host_labels(&self) -> [&str; 2] {
        [&self.host, "local"]
    }

    fn txt_data(&self) -> Vec<u8> {
        let mut data = Vec::new();
        for line in &self.txt {
            let b = line.as_bytes();
            if b.is_empty() || b.len() > MAX_TXT {
                continue;
            }
            data.push(b.len() as u8);
            data.extend_from_slice(b);
        }
        if data.is_empty() {
            data.push(0);
        }
        data
    }

    fn write(&self, out: &mut Vec<u8>, w: Want, ttl: u32) -> u16 {
        let mut n = 0;
        if w.meta {
            put_record(out, &META, TYPE_PTR, false, ttl, &name_bytes(&SERVICE));
            n += 1;
        }
        if w.ptr {
            put_record(
                out,
                &SERVICE,
                TYPE_PTR,
                false,
                ttl,
                &name_bytes(&self.instance_labels()),
            );
            n += 1;
        }
        if w.srv {
            let mut data = vec![0, 0, 0, 0];
            data.extend_from_slice(&self.port.to_be_bytes());
            put_name(&mut data, &self.host_labels());
            put_record(out, &self.instance_labels(), TYPE_SRV, true, ttl, &data);
            n += 1;
        }
        if w.txt {
            put_record(
                out,
                &self.instance_labels(),
                TYPE_TXT,
                true,
                ttl,
                &self.txt_data(),
            );
            n += 1;
        }
        if w.a {
            if let Some(a) = self.addr {
                put_record(out, &self.host_labels(), TYPE_A, true, ttl, &a.octets());
                n += 1;
            }
        }
        n
    }

    fn packet(&self, answers: Want, extra: Want, ttl: u32) -> Option<Vec<u8>> {
        let mut out = vec![0u8; 12];
        out[2] = 0x84;
        let an = self.write(&mut out, answers, ttl);
        let ar = self.write(&mut out, extra, ttl);
        if an == 0 || out.len() > MAX_RESPONSE {
            return None;
        }
        out[6..8].copy_from_slice(&an.to_be_bytes());
        out[10..12].copy_from_slice(&ar.to_be_bytes());
        Some(out)
    }

    /// The answer to the questions of a query, or `None` when none of them is about this server.
    pub fn answer(&self, questions: &[Question]) -> Option<Vec<u8>> {
        let mut answers = Want::default();
        let mut extra = Want::default();
        for q in questions {
            let any = q.qtype == TYPE_ANY;
            if same(&q.name, &META) && (q.qtype == TYPE_PTR || any) {
                answers.meta = true;
            } else if same(&q.name, &SERVICE) && (q.qtype == TYPE_PTR || any) {
                answers.ptr = true;
                extra.srv = true;
                extra.txt = true;
                extra.a = true;
            } else if same(&q.name, &self.instance_labels()) {
                if q.qtype == TYPE_SRV || any {
                    answers.srv = true;
                    extra.a = true;
                }
                if q.qtype == TYPE_TXT || any {
                    answers.txt = true;
                }
            } else if same(&q.name, &self.host_labels()) && (q.qtype == TYPE_A || any) {
                answers.a = true;
            }
        }
        // what is already an answer is not said again as an addition
        extra.meta = false;
        extra.ptr &= !answers.ptr;
        extra.srv &= !answers.srv;
        extra.txt &= !answers.txt;
        extra.a &= !answers.a;
        if self.addr.is_none() {
            answers.a = false;
            extra.a = false;
        }
        self.packet(answers, extra, TTL)
    }

    /// The packet the server sends by itself when it starts, every minute while it is on and, with `ttl` 0, when it is turned off.
    pub fn announcement(&self, ttl: u32) -> Option<Vec<u8>> {
        let all = Want {
            meta: false,
            ptr: true,
            srv: true,
            txt: true,
            a: true,
        };
        self.packet(all, Want::default(), ttl)
    }
}

/// The address of this computer on the network it reaches the multicast group by. Nothing is sent: connecting a UDP socket only asks the system for the route.
pub fn local_ipv4() -> Option<Ipv4Addr> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    socket.connect((GROUP, MDNS_PORT)).ok()?;
    match socket.local_addr().ok()? {
        SocketAddr::V4(a) if !a.ip().is_unspecified() && !a.ip().is_loopback() => Some(*a.ip()),
        _ => None,
    }
}

fn computer_name() -> String {
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_default()
}

fn open_socket() -> std::io::Result<UdpSocket> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, MDNS_PORT))?;
    socket.join_multicast_v4(&GROUP, &Ipv4Addr::UNSPECIFIED)?;
    socket.set_multicast_loop_v4(false)?;
    socket.set_multicast_ttl_v4(255)?;
    socket.set_read_timeout(Some(Duration::from_millis(500)))?;
    Ok(socket)
}

fn send(socket: &UdpSocket, packet: &[u8], to: SocketAddr) {
    let _ = socket.send_to(packet, to);
}

/// Starts the thread that answers. It reads the switch each half second, so turning it on or off in Studio takes effect at once; `bound` is the address the server listens on.
pub fn spawn(state: Arc<AppState>, bound: SocketAddr) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || run(&state, bound))
}

fn run(state: &Arc<AppState>, bound: SocketAddr) {
    let group = SocketAddr::from((GROUP, MDNS_PORT));
    let mut socket: Option<UdpSocket> = None;
    let mut announce: Option<Announce> = None;
    let mut last_try: Option<Instant> = None;
    let mut last_announce = Instant::now();
    let mut window = Instant::now();
    let mut sent = 0u32;
    let mut said_loopback = false;
    loop {
        let on = state.policy.get().mdns_announce;
        if !on {
            if let (Some(s), Some(a)) = (socket.take(), announce.take()) {
                if let Some(p) = a.announcement(0) {
                    send(&s, &p, group);
                }
                println!("eh3d-server: the announcement on the local network is off");
            }
            said_loopback = false;
            std::thread::sleep(Duration::from_millis(500));
            continue;
        }
        if bound.ip().is_loopback() {
            if !said_loopback {
                eprintln!("eh3d-server: mdns_announce is on but the server listens on this computer only; it is not announced (open the network first)");
                said_loopback = true;
            }
            std::thread::sleep(Duration::from_millis(500));
            continue;
        }
        if socket.is_none() {
            if last_try.is_some_and(|t| t.elapsed() < RETRY_BIND) {
                std::thread::sleep(Duration::from_millis(500));
                continue;
            }
            last_try = Some(Instant::now());
            match open_socket() {
                Ok(s) => {
                    let a = Announce::new(
                        &state.cfg.mdns_name,
                        &computer_name(),
                        bound,
                        local_ipv4(),
                        env!("CARGO_PKG_VERSION"),
                    );
                    if let Some(p) = a.announcement(TTL) {
                        send(&s, &p, group);
                    }
                    println!(
                        "eh3d-server: announcing '{}' as _eh3d._tcp on the local network",
                        a.instance
                    );
                    last_announce = Instant::now();
                    announce = Some(a);
                    socket = Some(s);
                }
                Err(e) => {
                    eprintln!("eh3d-server: cannot answer on the local network (port {MDNS_PORT}: {e}); another program may own it, see docs/DEPLOY.md for Avahi");
                    continue;
                }
            }
        }
        let (Some(s), Some(a)) = (socket.as_ref(), announce.as_ref()) else {
            continue;
        };
        if last_announce.elapsed() >= REANNOUNCE {
            if let Some(p) = a.announcement(TTL) {
                send(s, &p, group);
            }
            last_announce = Instant::now();
        }
        let mut buf = [0u8; MAX_PACKET + 1];
        let Ok((n, from)) = s.recv_from(&mut buf) else {
            continue;
        };
        if window.elapsed() >= Duration::from_secs(1) {
            window = Instant::now();
            sent = 0;
        }
        if sent >= MAX_ANSWERS_PER_SECOND {
            continue;
        }
        let Some(questions) = parse_query(&buf[..n.min(MAX_PACKET + 1)]) else {
            continue;
        };
        if let Some(reply) = a.answer(&questions) {
            // a program that is not a responder (it asks from another port) is answered to itself, as RFC 6762 says
            let to = if from.port() != MDNS_PORT || questions.iter().any(|q| q.unicast) {
                from
            } else {
                group
            };
            send(s, &reply, to);
            sent += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn announce() -> Announce {
        Announce::new(
            "Bench server",
            "Taller-PC",
            "0.0.0.0:18200".parse().unwrap(),
            Some(Ipv4Addr::new(192, 168, 0, 40)),
            "0.1.9",
        )
    }

    fn query(labels: &[&str], qtype: u16, class: u16) -> Vec<u8> {
        let mut p = vec![0u8; 12];
        p[5] = 1;
        put_name(&mut p, labels);
        p.extend_from_slice(&qtype.to_be_bytes());
        p.extend_from_slice(&class.to_be_bytes());
        p
    }

    fn counts(p: &[u8]) -> (u16, u16) {
        (
            u16::from_be_bytes([p[6], p[7]]),
            u16::from_be_bytes([p[10], p[11]]),
        )
    }

    #[test]
    fn labels_are_cleaned_and_cut() {
        assert_eq!(clean_label("a.b\u{7}c", "x"), "abc");
        assert_eq!(clean_label("...", "fallback"), "fallback");
        assert_eq!(clean_label(&"e".repeat(100), "x").len(), 63);
        // a cut never splits a character
        let long = "\u{e9}".repeat(40);
        let cut = clean_label(&long, "x");
        assert!(cut.len() <= 63 && cut.chars().all(|c| c == '\u{e9}'));
        assert_eq!(host_label("-Taller_PC.local-"), "tallerpclocal");
        assert_eq!(host_label("___"), "eh3d-server");
        assert_eq!(host_label(""), "eh3d-server");
    }

    #[test]
    fn the_instance_has_a_name_of_its_own_or_one_made_from_the_computer() {
        assert_eq!(announce().instance, "Bench server");
        let a = Announce::new(
            "  ",
            "Taller-PC",
            "0.0.0.0:18200".parse().unwrap(),
            None,
            "0.1.9",
        );
        assert_eq!(a.instance, "EH3D Server taller-pc");
        assert_eq!((a.port, a.host.as_str()), (18200, "taller-pc"));
    }

    #[test]
    fn the_address_comes_from_where_the_server_listens_and_is_never_made_up() {
        let local = Some(Ipv4Addr::new(10, 0, 0, 7));
        let all = Announce::new("x", "h", "0.0.0.0:18200".parse().unwrap(), local, "1");
        assert_eq!(all.addr, local);
        let named = Announce::new("x", "h", "192.168.1.9:18200".parse().unwrap(), local, "1");
        assert_eq!(named.addr, Some(Ipv4Addr::new(192, 168, 1, 9)));
        let none = Announce::new("x", "h", "0.0.0.0:18200".parse().unwrap(), None, "1");
        assert_eq!(none.addr, None);
        let six = Announce::new("x", "h", "[::]:18200".parse().unwrap(), local, "1");
        assert_eq!(six.addr, None);
    }

    #[test]
    fn a_question_for_the_service_gets_the_pointer_and_the_rest_as_additions() {
        let a = announce();
        let q = parse_query(&query(&SERVICE, TYPE_PTR, 1)).unwrap();
        let reply = a.answer(&q).expect("answered");
        assert_eq!(reply[2], 0x84, "a response that speaks with authority");
        assert_eq!(counts(&reply), (1, 3));
        assert!(reply.len() <= MAX_RESPONSE);
        let text = String::from_utf8_lossy(&reply).to_string();
        assert!(
            text.contains("_eh3d") && text.contains("Bench server") && text.contains("taller-pc")
        );
        assert!(text.contains("path=/api/v1") && text.contains("version=0.1.9"));
        // the port is in the SRV record
        assert!(reply.windows(2).any(|w| w == 18200u16.to_be_bytes()));
        // the address of the computer is in the A record
        assert!(reply.windows(4).any(|w| w == [192, 168, 0, 40]));
    }

    #[test]
    fn without_an_address_no_address_record_is_sent() {
        let mut a = announce();
        a.addr = None;
        let q = parse_query(&query(&SERVICE, TYPE_PTR, 1)).unwrap();
        assert_eq!(counts(&a.answer(&q).unwrap()), (1, 2));
        let q = parse_query(&query(&["taller-pc", "local"], TYPE_A, 1)).unwrap();
        assert!(a.answer(&q).is_none());
    }

    #[test]
    fn the_instance_and_the_host_are_answered_and_the_case_does_not_matter() {
        let a = announce();
        let q = parse_query(&query(
            &["BENCH SERVER", "_EH3D", "_tcp", "local"],
            TYPE_SRV,
            1,
        ))
        .unwrap();
        assert_eq!(counts(&a.answer(&q).unwrap()), (1, 1));
        let q = parse_query(&query(
            &["bench server", "_eh3d", "_tcp", "local"],
            TYPE_ANY,
            1,
        ))
        .unwrap();
        assert_eq!(counts(&a.answer(&q).unwrap()), (2, 1));
        let q = parse_query(&query(
            &["bench server", "_eh3d", "_tcp", "local"],
            TYPE_TXT,
            1,
        ))
        .unwrap();
        assert_eq!(counts(&a.answer(&q).unwrap()), (1, 0));
        let q = parse_query(&query(&["Taller-PC", "local"], TYPE_A, 1)).unwrap();
        assert_eq!(counts(&a.answer(&q).unwrap()), (1, 0));
    }

    #[test]
    fn the_list_of_services_names_ours() {
        let a = announce();
        let q = parse_query(&query(&META, TYPE_PTR, 1)).unwrap();
        let reply = a.answer(&q).unwrap();
        assert_eq!(counts(&reply), (1, 0));
        assert!(String::from_utf8_lossy(&reply).contains("_eh3d"));
    }

    #[test]
    fn what_is_not_about_the_server_is_not_answered() {
        let a = announce();
        for (labels, qtype) in [
            (vec!["_http", "_tcp", "local"], TYPE_PTR),
            (vec!["other", "local"], TYPE_A),
            (vec!["_eh3d", "_tcp", "local"], TYPE_A),
            (vec!["bench server", "_eh3d", "_tcp", "local"], TYPE_A),
        ] {
            let q = parse_query(&query(&labels, qtype, 1)).unwrap();
            assert!(a.answer(&q).is_none(), "{labels:?} {qtype}");
        }
    }

    #[test]
    fn the_unicast_bit_is_read_and_does_not_change_the_name() {
        let q = parse_query(&query(&SERVICE, TYPE_PTR, 0x8001)).unwrap();
        assert!(q[0].unicast && same(&q[0].name, &SERVICE));
        let q = parse_query(&query(&SERVICE, TYPE_PTR, 1)).unwrap();
        assert!(!q[0].unicast);
    }

    #[test]
    fn packets_that_are_not_queries_or_that_are_wrong_are_dropped() {
        let good = query(&SERVICE, TYPE_PTR, 1);
        assert!(parse_query(&good).is_some());
        // a response
        let mut response = good.clone();
        response[2] = 0x84;
        assert!(parse_query(&response).is_none());
        // another opcode
        let mut opcode = good.clone();
        opcode[2] = 0x28;
        assert!(parse_query(&opcode).is_none());
        // nothing asked, too short, cut in the middle, too big
        let mut empty = good.clone();
        empty[5] = 0;
        assert!(parse_query(&empty).is_none());
        assert!(parse_query(&good[..8]).is_none());
        assert!(parse_query(&good[..good.len() - 3]).is_none());
        assert!(parse_query(&vec![0u8; MAX_PACKET + 1]).is_none());
        // more questions than the limit
        let mut many = vec![0u8; 12];
        many[5] = (MAX_QUESTIONS + 1) as u8;
        for _ in 0..=MAX_QUESTIONS {
            put_name(&mut many, &SERVICE);
            many.extend_from_slice(&[0, 12, 0, 1]);
        }
        assert!(parse_query(&many).is_none());
        // garbage
        assert!(parse_query(&[0xFF; 40]).is_none());
        assert!(parse_query(&[]).is_none());
    }

    #[test]
    fn a_pointer_that_loops_does_not_hang_and_a_pointer_to_a_name_works() {
        // the name points at itself
        let mut p = vec![0u8; 12];
        p[5] = 1;
        p.extend_from_slice(&[0xC0, 12, 0, 12, 0, 1]);
        assert!(parse_query(&p).is_none());
        // a second question that points back at the first name
        let mut p = vec![0u8; 12];
        p[5] = 2;
        put_name(&mut p, &SERVICE);
        p.extend_from_slice(&[0, 12, 0, 1]);
        p.extend_from_slice(&[0xC0, 12, 0, 12, 0, 1]);
        let q = parse_query(&p).unwrap();
        assert_eq!(q.len(), 2);
        assert!(same(&q[1].name, &SERVICE));
        // a label longer than allowed
        let mut p = vec![0u8; 12];
        p[5] = 1;
        p.push(70);
        p.extend_from_slice(&[b'a'; 70]);
        p.extend_from_slice(&[0, 0, 12, 0, 1]);
        assert!(parse_query(&p).is_none());
    }

    #[test]
    fn the_announcement_says_everything_and_the_goodbye_has_no_time_to_live() {
        let a = announce();
        let hello = a.announcement(TTL).unwrap();
        assert_eq!(counts(&hello), (4, 0));
        let bye = a.announcement(0).unwrap();
        assert_eq!(counts(&bye), (4, 0));
        assert_eq!(hello.len(), bye.len());
        assert_ne!(hello, bye);
        // the TTL of the first record (after the name, the type and the class) is 120, and 0 in the goodbye
        let at = 12 + 18 + 4;
        assert_eq!(
            u32::from_be_bytes(hello[at..at + 4].try_into().unwrap()),
            TTL
        );
        assert_eq!(u32::from_be_bytes(bye[at..at + 4].try_into().unwrap()), 0);
    }

    #[test]
    fn a_long_name_still_makes_a_packet_within_the_limit() {
        let mut a = announce();
        a.instance = "n".repeat(300);
        a.host = "h".repeat(300);
        a.txt = (0..30)
            .map(|i| format!("k{i}={}", "v".repeat(150)))
            .collect();
        let q = parse_query(&query(&SERVICE, TYPE_PTR, 1)).unwrap();
        // either it fits or nothing is sent; it is never bigger than the limit
        if let Some(p) = a.answer(&q) {
            assert!(p.len() <= MAX_RESPONSE);
        }
        assert!(a.announcement(TTL).is_none_or(|p| p.len() <= MAX_RESPONSE));
    }

    #[test]
    fn a_txt_line_that_is_too_long_is_left_out() {
        let mut a = announce();
        a.txt = vec!["x".repeat(MAX_TXT + 1), "ok=1".to_string()];
        assert_eq!(a.txt_data(), b"\x04ok=1".to_vec());
        a.txt = Vec::new();
        assert_eq!(a.txt_data(), vec![0]);
    }
}
