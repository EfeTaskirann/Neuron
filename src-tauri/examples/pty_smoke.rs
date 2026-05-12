//! Standalone PTY smoke test — spawns claude.exe under portable-pty
//! and prints whatever it emits across a 6-second window, optionally
//! poking stdin to see if the REPL is waiting for an Enter to render.
//!
//! Diagnoses two failure modes:
//!   (a) claude emits NOTHING under portable-pty — TTY-detection issue
//!   (b) claude emits a few bytes (e.g. mode escapes) and then waits
//!       for keystrokes before painting the banner — fixable by sending
//!       a `\r` after a short delay, by setting `TERM`, or by
//!       triggering a resize.
//!
//! Run with:
//!   $env:CARGO_TARGET_DIR='src-tauri\target-smoke'
//!   cargo run --manifest-path src-tauri/Cargo.toml --example pty_smoke
//!
//! Optional first arg: path to a claude binary (defaults to the npm
//! install layout).

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use portable_pty::{native_pty_system, CommandBuilder, PtySize};

fn hex_dump(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 4);
    for (i, b) in bytes.iter().enumerate() {
        if i % 16 == 0 {
            if i > 0 {
                out.push('\n');
            }
            out.push_str(&format!("{:04x}  ", i));
        }
        out.push_str(&format!("{:02x} ", b));
    }
    out
}

fn ascii_dump(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    for &b in bytes {
        if (0x20..0x7f).contains(&b) {
            out.push(b as char);
        } else if b == b'\n' {
            out.push_str("\\n\n");
        } else if b == b'\r' {
            out.push_str("\\r");
        } else if b == 0x1b {
            out.push_str("\\e");
        } else if b == 0x07 {
            out.push_str("\\a");
        } else if b == 0x08 {
            out.push_str("\\b");
        } else if b == 0x09 {
            out.push_str("\\t");
        } else {
            out.push_str(&format!("\\x{:02x}", b));
        }
    }
    out
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let claude_path = args.get(1).cloned().unwrap_or_else(|| {
        r"C:\Users\efeta\AppData\Roaming\npm\node_modules\@anthropic-ai\claude-code\bin\claude.exe"
            .to_string()
    });
    // Optional flag toggles: set NEURON_SMOKE_TERM=1 to inject
    // TERM=xterm-256color, NEURON_SMOKE_RESIZE=1 to send a SIGWINCH
    // equivalent mid-flight, NEURON_SMOKE_POKE=1 to send a `\r` at 800ms.
    let inject_term = std::env::var("NEURON_SMOKE_TERM")
        .map(|v| v == "1")
        .unwrap_or(false);
    let do_resize = std::env::var("NEURON_SMOKE_RESIZE")
        .map(|v| v == "1")
        .unwrap_or(false);
    let do_poke = std::env::var("NEURON_SMOKE_POKE")
        .map(|v| v == "1")
        .unwrap_or(false);

    eprintln!(
        "[smoke] spawning: {claude_path} --dangerously-skip-permissions  \
         (TERM={inject_term}, resize={do_resize}, poke-stdin={do_poke})"
    );
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: 30,
            cols: 120,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("openpty");

    let mut cmd = CommandBuilder::new(&claude_path);
    cmd.arg("--dangerously-skip-permissions");
    cmd.cwd(std::env::current_dir().unwrap());
    cmd.set_controlling_tty(true);
    if inject_term {
        cmd.env("TERM", "xterm-256color");
    }

    let mut child = pair.slave.spawn_command(cmd).expect("spawn");
    drop(pair.slave);

    // Reader runs on its own thread so the main thread can write to
    // stdin / resize without blocking on a stalled `read`. The writer
    // is shared (Arc<Mutex<…>>) because the reader thread needs to send
    // DSR-CPR responses back through it, while the main thread issues
    // the scripted nudges (`\r`, `ping\r`).
    let mut reader = pair.master.try_clone_reader().expect("reader");
    let writer = pair.master.take_writer().expect("writer");
    let writer = Arc::new(Mutex::new(writer));
    let writer_for_reader = Arc::clone(&writer);
    let collected = Arc::new(Mutex::new(Vec::<u8>::new()));
    let collected_for_thread = Arc::clone(&collected);
    // Enable to verify the DSR-CPR auto-responder fixes the silent-pane
    // bug. Default-on now that we know what the problem is.
    let auto_respond_dsr = std::env::var("NEURON_SMOKE_NO_DSR")
        .map(|v| v != "1")
        .unwrap_or(true);
    let reader_thread = thread::spawn(move || {
        let mut buf = vec![0u8; 4096];
        let mut pending: Vec<u8> = Vec::new();
        loop {
            match reader.read(&mut buf) {
                Ok(0) => {
                    eprintln!("[smoke/reader] EOF");
                    break;
                }
                Ok(n) => {
                    pending.extend_from_slice(&buf[..n]);
                    let dsr_count = if auto_respond_dsr {
                        extract_dsr_cpr_queries(&mut pending)
                    } else {
                        0
                    };
                    if dsr_count > 0 {
                        if let Ok(mut w) = writer_for_reader.lock() {
                            for _ in 0..dsr_count {
                                let _ = w.write_all(b"\x1b[1;1R");
                            }
                            let _ = w.flush();
                        }
                        eprintln!(
                            "[smoke/reader] DSR-CPR x{dsr_count} answered at t={}ms",
                            global_t_ms()
                        );
                    }
                    let mut g = collected_for_thread.lock().expect("lock");
                    g.extend_from_slice(&pending);
                    pending.clear();
                    eprintln!(
                        "[smoke/reader] +{n} bytes (total {}) at t={}ms",
                        g.len(),
                        global_t_ms()
                    );
                }
                Err(e) => {
                    eprintln!("[smoke/reader] read err: {e}");
                    break;
                }
            }
        }
    });

    let write_main = |bytes: &[u8]| {
        if let Ok(mut w) = writer.lock() {
            let _ = w.write_all(bytes);
            let _ = w.flush();
        }
    };

    // Timeline:
    //   0ms    spawned
    //   800ms  (optional) send `\r` to nudge claude
    //   1500ms (optional) issue a resize SIGWINCH
    //   3000ms (optional) send "ping\r" as a user prompt
    //   6000ms tear down + dump
    if do_poke {
        thread::sleep(Duration::from_millis(800));
        eprintln!("[smoke/main] writing `\\r` to stdin at t={}ms", global_t_ms());
        write_main(b"\r");
    }
    if do_resize {
        thread::sleep(Duration::from_millis(700));
        eprintln!("[smoke/main] resize → 28x118 at t={}ms", global_t_ms());
        let _ = pair.master.resize(PtySize {
            rows: 28,
            cols: 118,
            pixel_width: 0,
            pixel_height: 0,
        });
    }
    thread::sleep(Duration::from_millis(3000));
    eprintln!("[smoke/main] sending `ping\\r` at t={}ms", global_t_ms());
    write_main(b"ping\r");

    thread::sleep(Duration::from_millis(2000));

    eprintln!("[smoke/main] deadline reached, killing child at t={}ms", global_t_ms());
    let _ = child.kill();

    // Give the reader thread up to 500ms to drain after kill.
    let drain_until = Instant::now() + Duration::from_millis(500);
    while Instant::now() < drain_until && !reader_thread.is_finished() {
        thread::sleep(Duration::from_millis(50));
    }

    let final_buf = collected.lock().expect("final lock").clone();
    eprintln!("[smoke] ---- {} bytes total ----", final_buf.len());
    eprintln!("[smoke] ASCII (control chars escaped):");
    eprintln!("{}", ascii_dump(&final_buf));
    eprintln!("[smoke] HEX:");
    eprintln!("{}", hex_dump(&final_buf));
    eprintln!("[smoke] ---- end ----");

    let _ = child.wait();
}

fn global_t_ms() -> u128 {
    use std::sync::OnceLock;
    static START: OnceLock<Instant> = OnceLock::new();
    let start = START.get_or_init(Instant::now);
    start.elapsed().as_millis()
}

/// Mirror of `sidecar::terminal::extract_dsr_cpr_queries`. Strips every
/// `\x1b[6n` query from the byte buffer in place and returns the count
/// so the caller can write back one `\x1b[1;1R` reply per query.
fn extract_dsr_cpr_queries(buf: &mut Vec<u8>) -> usize {
    const QUERY: &[u8] = b"\x1b[6n";
    let mut count = 0;
    let mut from = 0;
    while from + QUERY.len() <= buf.len() {
        if let Some(rel) = buf[from..]
            .windows(QUERY.len())
            .position(|w| w == QUERY)
        {
            let abs = from + rel;
            buf.drain(abs..abs + QUERY.len());
            count += 1;
            from = abs;
        } else {
            break;
        }
    }
    count
}
