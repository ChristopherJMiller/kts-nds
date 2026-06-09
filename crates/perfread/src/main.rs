//! Pull the `PerfBlob` frame-time ring out of a running DS ROM via desmume's
//! gdbstub. Companion to `bevy_nds_diagnostics::PerfBlob` and `just preview`.
//!
//! Usage: `perfread --port 9999 [--addr 0x02321bd8] [--run-ms 3000]`
//!
//! desmume's gdbstub launches the emulator *paused* and only runs ARM9 code
//! when the debugger sends `c` (continue). `--run-ms N` tells perfread to send
//! `c`, sleep `N` ms (so the demo accumulates frame-time samples), then
//! interrupt with a BREAK (0x03), and then read the `PerfBlob`. Without it,
//! the read happens against whatever state the gdbstub paused on at launch —
//! which means a ring full of zeros if the ROM hasn't been told to start yet.
//!
//! With `--addr` set, reads the blob directly from that ARM9 main-RAM address.
//! Without it, scans `0x02000000..0x02400000` for the magic header `b"BVDS"` —
//! handy when the symbol moves between builds. The address-known path is much
//! faster (a single 1 KB read), so `just preview` passes it explicitly after
//! fishing the symbol out of the ELF with `nm`.
//!
//! Output (one line, space-separated key=value pairs) — designed to be both
//! grep-friendly and forwarded into a build log:
//!
//! ```text
//! samples=180 min=16.6ms avg=33.1ms p50=33.2ms p95=33.4ms fps_avg=30.2
//! ```

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

/// Mirrors `bevy_nds_diagnostics::PERF_MAGIC`.
const PERF_MAGIC: &[u8; 4] = b"BVDS";
/// Mirrors `bevy_nds_diagnostics::PERF_VERSION`.
const PERF_VERSION: u32 = 1;
/// Mirrors `bevy_nds_diagnostics::PERF_RING_LEN`.
const PERF_RING_LEN: usize = 256;
/// `magic(4) + version(4) + head(4) + ring_len(4) + written(8) + ring(256*4)`.
const PERF_BLOB_SIZE: usize = 4 + 4 + 4 + 4 + 8 + PERF_RING_LEN * 4;

/// ARM9 main RAM (the DS's 4 MB of work RAM). The scanner sweeps this range.
const MAIN_RAM_BASE: u32 = 0x0200_0000;
const MAIN_RAM_LEN: u32 = 0x0040_0000;

fn main() {
    let opts = match parse_args() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("perfread: {e}");
            eprintln!(
                "usage: perfread --port PORT [--addr 0xHEX] [--connect-timeout MS] [--run-ms MS]"
            );
            std::process::exit(2);
        }
    };

    let mut conn = match Connection::open(opts.port, opts.connect_timeout) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("perfread: connect to localhost:{}: {}", opts.port, e);
            std::process::exit(1);
        }
    };

    // Drive the initial handshake. desmume's stub will reply '+' to our '$qSupported'
    // packet; the empty body it returns ("$#00") is fine — we only need the core `m`
    // command, which every implementation supports.
    if let Err(e) = conn.handshake() {
        eprintln!("perfread: handshake: {e}");
        std::process::exit(1);
    }

    if opts.run_ms > 0 {
        if let Err(e) = conn.run_for(Duration::from_millis(opts.run_ms)) {
            eprintln!("perfread: run_for: {e}");
            std::process::exit(1);
        }
    }

    let addr = match opts.addr {
        Some(a) => a,
        None => match scan_for_magic(&mut conn) {
            Ok(a) => a,
            Err(e) => {
                eprintln!("perfread: scan for magic: {e}");
                std::process::exit(1);
            }
        },
    };

    let bytes = match conn.read_mem(addr, PERF_BLOB_SIZE) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("perfread: read PerfBlob at {:#010x}: {}", addr, e);
            std::process::exit(1);
        }
    };

    // Don't bother detaching cleanly — just dropping the socket leaves desmume
    // running (the stub treats a dropped connection as "client gone"). Saves
    // us a `D` packet and the timing it would need.

    let blob = match Blob::decode(&bytes) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("perfread: decode: {e}");
            std::process::exit(1);
        }
    };

    let stats = Stats::from_blob(&blob);
    println!("{stats}");
}

#[derive(Debug)]
struct Opts {
    port: u16,
    addr: Option<u32>,
    connect_timeout: Duration,
    run_ms: u64,
}

fn parse_args() -> Result<Opts, String> {
    let mut port: Option<u16> = None;
    let mut addr: Option<u32> = None;
    let mut connect_timeout = Duration::from_millis(2000);
    let mut run_ms: u64 = 0;

    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--port" => {
                let v = args.next().ok_or("--port needs a value")?;
                port = Some(v.parse().map_err(|e| format!("--port: {e}"))?);
            }
            "--addr" => {
                let v = args.next().ok_or("--addr needs a value")?;
                let s = v.strip_prefix("0x").unwrap_or(&v);
                addr = Some(u32::from_str_radix(s, 16).map_err(|e| format!("--addr: {e}"))?);
            }
            "--connect-timeout" => {
                let v = args.next().ok_or("--connect-timeout needs a value (ms)")?;
                let ms: u64 = v.parse().map_err(|e| format!("--connect-timeout: {e}"))?;
                connect_timeout = Duration::from_millis(ms);
            }
            "--run-ms" => {
                let v = args.next().ok_or("--run-ms needs a value (ms)")?;
                run_ms = v.parse().map_err(|e| format!("--run-ms: {e}"))?;
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(Opts {
        port: port.ok_or("--port is required")?,
        addr,
        connect_timeout,
        run_ms,
    })
}

// --- gdb-remote serial protocol ----------------------------------------------

/// A live connection to desmume's gdbstub. Speaks the minimum subset of the
/// gdb Remote Serial Protocol we need: send a packet, read a packet, ack.
struct Connection {
    stream: TcpStream,
    rx: Vec<u8>,
}

impl Connection {
    fn open(port: u16, timeout: Duration) -> std::io::Result<Self> {
        // Poll the port — desmume opens it before the ROM has loaded much state,
        // but only after the SDL/Xvfb chatter finishes, so a fixed `sleep` from
        // the caller racing with a single connect() is brittle. Retry until the
        // timeout expires.
        let deadline = std::time::Instant::now() + timeout;
        let addr = format!("127.0.0.1:{port}");
        loop {
            match TcpStream::connect_timeout(&addr.parse().unwrap(), Duration::from_millis(200)) {
                Ok(stream) => {
                    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
                    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
                    return Ok(Self {
                        stream,
                        rx: Vec::new(),
                    });
                }
                Err(_) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(e) => return Err(e),
            }
        }
    }

    fn handshake(&mut self) -> std::io::Result<()> {
        // RSP convention: the client acks any pending packet from the server,
        // then issues qSupported to negotiate. desmume sends nothing
        // unsolicited, so the leading '+' is a no-op; sending it anyway is
        // harmless and matches what real gdb does.
        self.stream.write_all(b"+")?;
        let _ = self.exchange(b"qSupported:multiprocess+")?;
        Ok(())
    }

    /// Unpause the emulator, sleep `duration`, then BREAK so we can read
    /// memory. desmume's gdbstub launches paused; without this the PerfBlob
    /// ring is whatever sat in `.data` at boot (zeros for the samples).
    ///
    /// Writes `$c#63` directly (continue), then a 0x03 BREAK byte to interrupt,
    /// then consumes the `Sxx` stop reply so the connection is back to a
    /// known-good state for subsequent reads.
    fn run_for(&mut self, duration: Duration) -> std::io::Result<()> {
        // `$c#63` is the gdb-remote "continue at current PC" packet. We do not
        // use `exchange` here because the stub will not reply until execution
        // halts — we drive that ourselves by sending the BREAK byte below.
        self.stream.write_all(b"$c#63")?;
        std::thread::sleep(duration);
        // BREAK: a single 0x03 byte, sent *outside* of any packet framing.
        self.stream.write_all(&[0x03])?;
        // The stub replies with `$Sxx#yy` (signalled stop). Consume it so the
        // next exchange call doesn't see it as an unexpected packet.
        loop {
            if take_packet(&mut self.rx).is_some() {
                self.stream.write_all(b"+")?;
                return Ok(());
            }
            let mut buf = [0u8; 256];
            let n = self.stream.read(&mut buf)?;
            if n == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "stub closed connection during run_for",
                ));
            }
            self.rx.extend_from_slice(&buf[..n]);
        }
    }

    /// Read `len` bytes of ARM9 memory starting at `addr`.
    fn read_mem(&mut self, addr: u32, len: usize) -> Result<Vec<u8>, String> {
        // desmume's gdbstub (0.9.13) caps `m` packet replies somewhere around
        // 512 bytes of payload. Anything bigger comes back as a buffer full of
        // NULs (looks like it allocates a too-small response slot and never
        // writes the hex into it). 256 bytes per chunk stays well under the
        // limit and keeps PerfBlob (~1 KB) at four reads.
        let mut out = Vec::with_capacity(len);
        let mut off = 0usize;
        while off < len {
            let chunk = std::cmp::min(256, len - off);
            let cmd = format!("m{:x},{:x}", addr + off as u32, chunk);
            let reply = self
                .exchange(cmd.as_bytes())
                .map_err(|e| format!("io: {e}"))?;
            if let Some(rest) = reply.strip_prefix(b"E") {
                let code = std::str::from_utf8(rest).unwrap_or("?");
                return Err(format!("stub returned error E{code} at +{off:x}"));
            }
            if reply.len() != 2 * chunk {
                return Err(format!(
                    "short read: asked {chunk} bytes, got {} hex chars at +{off:x}",
                    reply.len()
                ));
            }
            for i in 0..chunk {
                let hi = hex_val(reply[2 * i]).ok_or("bad hex")?;
                let lo = hex_val(reply[2 * i + 1]).ok_or("bad hex")?;
                out.push((hi << 4) | lo);
            }
            off += chunk;
        }
        Ok(out)
    }

    /// Send `body` as a `$body#XX` packet, swallow the stub's `+` ack, return
    /// the bytes inside the next `$...#XX` reply.
    fn exchange(&mut self, body: &[u8]) -> std::io::Result<Vec<u8>> {
        let sum: u8 = body.iter().fold(0u8, |a, &b| a.wrapping_add(b));
        let pkt = {
            let mut p = Vec::with_capacity(body.len() + 5);
            p.push(b'$');
            p.extend_from_slice(body);
            p.push(b'#');
            p.extend_from_slice(format!("{sum:02x}").as_bytes());
            p
        };
        self.stream.write_all(&pkt)?;
        // Read until we have one full packet body. Bring in more bytes as
        // needed; tolerate either an explicit '+' / '-' between packets.
        loop {
            if let Some(body) = take_packet(&mut self.rx) {
                // Ack the stub so it considers the exchange complete (matches
                // what real gdb does — and desmume sometimes withholds further
                // packets without it).
                self.stream.write_all(b"+")?;
                return Ok(body);
            }
            let mut buf = [0u8; 1024];
            let n = self.stream.read(&mut buf)?;
            if n == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "stub closed connection",
                ));
            }
            self.rx.extend_from_slice(&buf[..n]);
        }
    }
}

/// If `buf` contains a `$...#XX` packet, drain it (plus any leading acks) and
/// return the body. Returns `None` if a full packet is not yet present.
fn take_packet(buf: &mut Vec<u8>) -> Option<Vec<u8>> {
    // Skip leading ack characters.
    while let Some(&b) = buf.first() {
        if b == b'+' || b == b'-' {
            buf.remove(0);
        } else {
            break;
        }
    }
    let dollar = buf.iter().position(|&b| b == b'$')?;
    let hash = buf.iter().skip(dollar).position(|&b| b == b'#')? + dollar;
    // Need two checksum bytes after '#'.
    if buf.len() < hash + 3 {
        return None;
    }
    let body = buf[dollar + 1..hash].to_vec();
    buf.drain(..hash + 3);
    Some(body)
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Sweep main RAM in coarse chunks looking for the magic. Returns the address
/// of the magic header (the start of `PerfBlob`). Slower than passing `--addr`,
/// but lets the tool work standalone.
fn scan_for_magic(conn: &mut Connection) -> Result<u32, String> {
    // 64 KB chunks keep each gdb-remote read under desmume's typical packet
    // limit; ~64 reads cover the full 4 MB. Worst-case ~half a second.
    const CHUNK: u32 = 0x10000;
    let mut addr = MAIN_RAM_BASE;
    let end = MAIN_RAM_BASE + MAIN_RAM_LEN;
    while addr < end {
        let len = std::cmp::min(CHUNK, end - addr) as usize;
        let bytes = conn.read_mem(addr, len).map_err(|e| format!("read: {e}"))?;
        // Search for the magic with a stride of 1 — PerfBlob has 4-byte align,
        // but the cost is the same either way.
        for (i, w) in bytes.windows(PERF_MAGIC.len()).enumerate() {
            if w == PERF_MAGIC {
                return Ok(addr + i as u32);
            }
        }
        addr += CHUNK;
    }
    Err(format!(
        "magic {:?} not found in {:#010x}..{:#010x}",
        std::str::from_utf8(PERF_MAGIC).unwrap_or("?"),
        MAIN_RAM_BASE,
        end
    ))
}

// --- PerfBlob decode + stats -------------------------------------------------

#[cfg_attr(test, derive(Debug))]
struct Blob {
    samples: Vec<u32>,
}

impl Blob {
    fn decode(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() < PERF_BLOB_SIZE {
            return Err(format!(
                "short blob: got {} bytes, expected {}",
                bytes.len(),
                PERF_BLOB_SIZE
            ));
        }
        if &bytes[0..4] != PERF_MAGIC {
            return Err(format!("bad magic: {:02x?}", &bytes[0..4]));
        }
        let version = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
        if version != PERF_VERSION {
            return Err(format!(
                "unsupported PerfBlob version {version} (this tool knows {PERF_VERSION})"
            ));
        }
        let head = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
        let ring_len = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
        let written = u64::from_le_bytes(bytes[16..24].try_into().unwrap()) as usize;
        if ring_len != PERF_RING_LEN {
            return Err(format!(
                "ring_len mismatch: blob says {ring_len}, this tool was built for {PERF_RING_LEN}"
            ));
        }
        let ring_off = 24;
        let mut ring = Vec::with_capacity(ring_len);
        for i in 0..ring_len {
            let o = ring_off + i * 4;
            ring.push(u32::from_le_bytes(bytes[o..o + 4].try_into().unwrap()));
        }
        // Reconstruct in oldest -> newest order. While `written < ring_len`, the
        // valid samples are the first `written` slots. After it wraps, the ring
        // is `head..ring_len` followed by `0..head`.
        let samples = if written < ring_len {
            ring[..written].to_vec()
        } else {
            let mut s = Vec::with_capacity(ring_len);
            s.extend_from_slice(&ring[head..]);
            s.extend_from_slice(&ring[..head]);
            s
        };
        // Drop any zero placeholders just in case.
        let samples: Vec<u32> = samples.into_iter().filter(|&x| x != 0).collect();
        Ok(Self { samples })
    }
}

struct Stats {
    n: usize,
    min_us: u32,
    avg_us: f64,
    p50_us: u32,
    p95_us: u32,
}

impl Stats {
    fn from_blob(blob: &Blob) -> Self {
        let mut s = blob.samples.clone();
        if s.is_empty() {
            return Self {
                n: 0,
                min_us: 0,
                avg_us: 0.0,
                p50_us: 0,
                p95_us: 0,
            };
        }
        s.sort_unstable();
        let n = s.len();
        let sum: u64 = s.iter().map(|&x| x as u64).sum();
        Self {
            n,
            min_us: s[0],
            avg_us: sum as f64 / n as f64,
            p50_us: s[percentile_idx(n, 50)],
            p95_us: s[percentile_idx(n, 95)],
        }
    }
}

/// Pick the index for the `p`-th percentile of an `n`-element sorted slice.
/// Uses `floor(p/100 * n)` clamped to `0..n` — i.e. a "5% of samples are above
/// p95" interpretation, which is what you want when watching tail latency. For
/// the steady-state case all samples are equal so the exact index doesn't
/// matter; for the spike case (95 fast + 5 slow at p95) this picks the first
/// slow sample, not the last fast one.
fn percentile_idx(n: usize, p: u32) -> usize {
    ((p as usize * n) / 100).min(n - 1)
}

impl std::fmt::Display for Stats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.n == 0 {
            return write!(f, "samples=0 (PerfBlob ring is empty — ROM still booting?)");
        }
        let fps = 1_000_000.0 / self.avg_us;
        write!(
            f,
            "samples={} min={:.1}ms avg={:.1}ms p50={:.1}ms p95={:.1}ms fps_avg={:.1}",
            self.n,
            self.min_us as f64 / 1000.0,
            self.avg_us / 1000.0,
            self.p50_us as f64 / 1000.0,
            self.p95_us as f64 / 1000.0,
            fps,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode_blob(head: u32, written: u64, ring: &[u32; PERF_RING_LEN]) -> Vec<u8> {
        let mut v = Vec::with_capacity(PERF_BLOB_SIZE);
        v.extend_from_slice(PERF_MAGIC);
        v.extend_from_slice(&PERF_VERSION.to_le_bytes());
        v.extend_from_slice(&head.to_le_bytes());
        v.extend_from_slice(&(PERF_RING_LEN as u32).to_le_bytes());
        v.extend_from_slice(&written.to_le_bytes());
        for &x in ring {
            v.extend_from_slice(&x.to_le_bytes());
        }
        v
    }

    #[test]
    fn decodes_partially_filled_ring_in_order() {
        let mut ring = [0u32; PERF_RING_LEN];
        ring[0] = 16_667;
        ring[1] = 16_700;
        ring[2] = 33_000;
        let bytes = encode_blob(3, 3, &ring);
        let b = Blob::decode(&bytes).unwrap();
        assert_eq!(b.samples, vec![16_667, 16_700, 33_000]);
    }

    #[test]
    fn decodes_wrapped_ring_in_oldest_first_order() {
        // Ring has wrapped once: written = ring_len + 5, head points 5 in.
        let mut ring = [0u32; PERF_RING_LEN];
        for (i, slot) in ring.iter_mut().enumerate() {
            *slot = (i + 1) as u32;
        }
        // Pretend the next 5 writes overwrote the first 5 slots with marker values.
        ring[0] = 9001;
        ring[1] = 9002;
        ring[2] = 9003;
        ring[3] = 9004;
        ring[4] = 9005;
        let head = 5u32;
        let written = PERF_RING_LEN as u64 + 5;
        let bytes = encode_blob(head, written, &ring);
        let b = Blob::decode(&bytes).unwrap();
        // Oldest sample should be ring[head] = 6, newest should be 9005.
        assert_eq!(b.samples.first(), Some(&6));
        assert_eq!(b.samples.last(), Some(&9005));
        assert_eq!(b.samples.len(), PERF_RING_LEN);
    }

    #[test]
    fn stats_handle_steady_60fps() {
        let mut ring = [0u32; PERF_RING_LEN];
        for slot in &mut ring[..100] {
            *slot = 16_667;
        }
        let bytes = encode_blob(100, 100, &ring);
        let blob = Blob::decode(&bytes).unwrap();
        let s = Stats::from_blob(&blob);
        assert_eq!(s.n, 100);
        assert_eq!(s.min_us, 16_667);
        assert!((s.avg_us - 16_667.0).abs() < 0.001);
        assert_eq!(s.p50_us, 16_667);
        assert_eq!(s.p95_us, 16_667);
    }

    #[test]
    fn stats_p95_picks_a_tail_sample() {
        let mut ring = [0u32; PERF_RING_LEN];
        for slot in &mut ring[..100] {
            *slot = 16_667;
        }
        // 5 slow frames at 50 ms.
        for slot in &mut ring[95..100] {
            *slot = 50_000;
        }
        let bytes = encode_blob(100, 100, &ring);
        let blob = Blob::decode(&bytes).unwrap();
        let s = Stats::from_blob(&blob);
        assert_eq!(s.min_us, 16_667);
        // p95 of 100 samples is the 95th-ranked = first of the 5 tail outliers.
        assert_eq!(s.p95_us, 50_000);
    }

    #[test]
    fn rejects_bad_magic() {
        let mut bytes = encode_blob(0, 0, &[0; PERF_RING_LEN]);
        bytes[0] = b'X';
        let err = Blob::decode(&bytes).unwrap_err();
        assert!(err.contains("bad magic"), "got {err}");
    }

    #[test]
    fn rejects_unknown_version() {
        let mut bytes = encode_blob(0, 0, &[0; PERF_RING_LEN]);
        bytes[4] = 99; // version = 99
        let err = Blob::decode(&bytes).unwrap_err();
        assert!(err.contains("version"), "got {err}");
    }

    #[test]
    fn take_packet_handles_acks_and_split_reads() {
        // Two acks, a packet, and a trailing byte.
        let mut buf = b"++$qSupported#37x".to_vec();
        let body = take_packet(&mut buf).unwrap();
        assert_eq!(body, b"qSupported");
        assert_eq!(buf, b"x");
    }

    #[test]
    fn take_packet_returns_none_on_partial() {
        let mut buf = b"$qSup".to_vec();
        assert!(take_packet(&mut buf).is_none());
        // Buffer untouched.
        assert_eq!(buf, b"$qSup");
    }

    #[test]
    fn percentile_idx_basic() {
        // p95 of 100 -> idx 95 (the first sample above the 95th-percentile cut).
        assert_eq!(percentile_idx(100, 50), 50);
        assert_eq!(percentile_idx(100, 95), 95);
        assert_eq!(percentile_idx(100, 100), 99);
        // tiny edge cases
        assert_eq!(percentile_idx(1, 50), 0);
        assert_eq!(percentile_idx(2, 50), 1);
        assert_eq!(percentile_idx(2, 95), 1);
    }
}
