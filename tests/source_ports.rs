//! Source-port selection tests.
//!
//! A minimal HTTP/1.1 endpoint records the source port of every accepted
//! connection while the real `plumbrs` binary runs with `--local-port-range` or
//! `--rss-*`. The test fails if the upstream does not see the expected ports.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Source ports observed by the endpoint.
type SeenPorts = Arc<Mutex<Vec<u16>>>;

fn handle_conn(stream: std::net::TcpStream, seen: &SeenPorts) {
    if let Ok(peer) = stream.peer_addr() {
        seen.lock().unwrap().push(peer.port());
    }
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut writer = stream;
    // Serve one request per connection is enough; keep-alive loops are also
    // handled until EOF.
    loop {
        let mut request_line = String::new();
        match reader.read_line(&mut request_line) {
            Ok(0) => break,
            Err(_) => break,
            Ok(_) => {}
        }
        if request_line.trim().is_empty() {
            continue;
        }
        let mut content_length = 0usize;
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line).is_err() {
                return;
            }
            let line = line.trim();
            if line.is_empty() {
                break;
            }
            if let Some(value) = line.to_lowercase().strip_prefix("content-length:") {
                content_length = value.trim().parse().unwrap_or(0);
            }
        }
        if content_length > 0 {
            let mut buf = vec![0u8; content_length];
            if reader.read_exact(&mut buf).is_err() {
                return;
            }
        }
        if writer
            .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok")
            .is_err()
        {
            break;
        }
    }
}

/// Spawn the test endpoint on an ephemeral port; returns the port.
fn spawn_endpoint(seen: SeenPorts) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    listener.set_nonblocking(true).unwrap();
    std::thread::spawn(move || {
        for _ in 0..6000 {
            match listener.accept() {
                Ok((stream, _)) => {
                    stream.set_nonblocking(false).unwrap();
                    let seen = Arc::clone(&seen);
                    std::thread::spawn(move || handle_conn(stream, &seen));
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(_) => break,
            }
        }
    });
    port
}

fn run_plumbrs(extra_args: &[&str], port: u16) -> std::process::ExitStatus {
    let bin = env!("CARGO_BIN_EXE_plumbrs");
    let mut child = std::process::Command::new(bin)
        .args(extra_args)
        .arg(format!("http://127.0.0.1:{port}/"))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();

    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        match child.try_wait().unwrap() {
            Some(status) => return status,
            None if Instant::now() > deadline => {
                child.kill().ok();
                panic!("plumbrs timed out");
            }
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    }
}

/// Wait until the endpoint saw `n` connections (or time out).
fn wait_for(seen: &SeenPorts, n: usize) -> Vec<u16> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        {
            let ports = seen.lock().unwrap();
            if ports.len() >= n {
                return ports.clone();
            }
        }
        if Instant::now() > deadline {
            panic!(
                "endpoint saw {} connections, expected at least {n}",
                seen.lock().unwrap().len()
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn local_port_range_is_honored() {
    let seen: SeenPorts = Arc::new(Mutex::new(Vec::new()));
    let port = spawn_endpoint(Arc::clone(&seen));

    let status = run_plumbrs(
        &[
            "-t", "2", "-c", "4", "-r", "2", "-C", "hyper", "--local-port-range",
            "41200-41299",
        ],
        port,
    );
    assert!(status.success(), "plumbrs exited with {status}");

    let ports = wait_for(&seen, 4);
    assert_eq!(ports.len(), 4, "expected one connection per task, saw {ports:?}");
    let mut sorted = ports.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), 4, "source ports must be distinct, saw {ports:?}");
    for p in &sorted {
        assert!(
            (41200..=41299).contains(p),
            "source port {p} outside requested range"
        );
    }
}

#[test]
fn local_port_range_assigns_deterministically() {
    let seen: SeenPorts = Arc::new(Mutex::new(Vec::new()));
    let port = spawn_endpoint(Arc::clone(&seen));

    let status = run_plumbrs(
        &["-t", "1", "-c", "2", "-r", "1", "-C", "hyper", "--local-port-range", "42100-42101"],
        port,
    );
    assert!(status.success(), "plumbrs exited with {status}");

    let mut ports = wait_for(&seen, 2);
    ports.sort_unstable();
    assert_eq!(ports, vec![42100, 42101]);
}

const SAMPLE_RXFH: &str = "\
RX flow hash indirection table for eth0 with 2 RX ring(s):\n\
    0:      0     1     0     1     0     1     0     1\n\
RSS hash key:\n\
6d:5a:56:da:25:5b:0e:c2:41:67:25:3d:43:a3:8f:b0:d0:ca:2b:cb:ae:7b:30:b4:77:cb:2d:a3:30:28:6c:1d:36:5d:5b:02:e3:a6:6c:82\n\
RSS hash function:\n\
    toeplitz: on\n";

#[test]
fn rss_files_are_accepted() {
    let dir = std::env::temp_dir().join(format!("plumbrs-rss-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let rxfh = dir.join("rxfh.txt");
    std::fs::write(&rxfh, SAMPLE_RXFH).unwrap();
    let rxfh = rxfh.to_str().unwrap().to_owned();

    let seen: SeenPorts = Arc::new(Mutex::new(Vec::new()));
    let port = spawn_endpoint(Arc::clone(&seen));

    let key_arg = format!("@{}", rxfh);
    let indir_arg = format!("@{}", rxfh);
    let status = run_plumbrs(
        &[
            "-t",
            "1",
            "-c",
            "4",
            "-r",
            "2",
            "-C",
            "hyper",
            "--local-port-range",
            "41300-41399",
            "--rss-key",
            &key_arg,
            "--rss-indir",
            &indir_arg,
        ],
        port,
    );
    assert!(status.success(), "plumbrs exited with {status}");

    let ports = wait_for(&seen, 4);
    assert_eq!(ports.len(), 4);
    for p in &ports {
        assert!(
            (41300..=41399).contains(p),
            "source port {p} outside requested range"
        );
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[cfg(feature = "compio")]
#[test]
fn compio_local_port_range_is_honored() {
    let seen: SeenPorts = Arc::new(Mutex::new(Vec::new()));
    let port = spawn_endpoint(Arc::clone(&seen));

    let status = run_plumbrs(
        &[
            "-t", "1", "-c", "4", "-r", "2", "-C", "compio", "--local-port-range",
            "41400-41499",
        ],
        port,
    );
    assert!(status.success(), "plumbrs exited with {status}");

    let ports = wait_for(&seen, 4);
    assert_eq!(ports.len(), 4, "expected one connection per task, saw {ports:?}");
    let mut sorted = ports.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), 4, "source ports must be distinct, saw {ports:?}");
    for p in &sorted {
        assert!(
            (41400..=41499).contains(p),
            "source port {p} outside requested range"
        );
    }
}

#[test]
fn source_port_rejected_with_finite_rpc() {
    let seen: SeenPorts = Arc::new(Mutex::new(Vec::new()));
    let port = spawn_endpoint(Arc::clone(&seen));

    let status = run_plumbrs(
        &[
            "-c", "1", "-r", "1", "--rpc", "1", "-C", "hyper", "--local-port-range",
            "42200-42299",
        ],
        port,
    );
    assert!(
        !status.success(),
        "--local-port-range should be rejected with finite --rpc"
    );
}

#[test]
fn source_port_rejected_for_reqwest() {
    let seen: SeenPorts = Arc::new(Mutex::new(Vec::new()));
    let port = spawn_endpoint(Arc::clone(&seen));

    let status = run_plumbrs(
        &["-c", "1", "-r", "1", "-C", "reqwest", "--local-port-range", "42200-42299"],
        port,
    );
    assert!(
        !status.success(),
        "reqwest client should reject --local-port-range"
    );
}
