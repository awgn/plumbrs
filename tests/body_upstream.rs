//! Upstream body-delivery tests.
//!
//! A minimal HTTP/1.1 endpoint records the requests it receives (headers +
//! body) while the real `plumbrs` binary hammers it with POSTs carrying
//! `-b <body>`. The test fails if the upstream does not see the body bytes.
//!
//! Regression test: the compio/monoio/tokio-uring clients serialize with
//! `http_wire` and write bytes directly, so without an explicit
//! `Content-Length` the upstream treated every request as bodyless.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Default, PartialEq)]
struct CapturedRequest {
    content_length: Option<usize>,
    body: Vec<u8>,
}

fn read_request(reader: &mut BufReader<TcpStream>) -> std::io::Result<Option<CapturedRequest>> {
    let mut request_line = String::new();
    if reader.read_line(&mut request_line)? == 0 {
        return Ok(None); // EOF
    }
    if request_line.trim().is_empty() {
        return Ok(None);
    }

    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line)?;
        let line = line.trim();
        if line.is_empty() {
            break;
        }
        if let Some(value) = line.to_lowercase().strip_prefix("content-length:") {
            content_length = value.trim().parse().ok();
        }
    }

    let mut body = Vec::new();
    if let Some(len) = content_length {
        reader.take(len as u64).read_to_end(&mut body)?;
    }

    Ok(Some(CapturedRequest {
        content_length,
        body,
    }))
}

fn handle_conn(stream: TcpStream, captured: &Arc<Mutex<Vec<CapturedRequest>>>) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut writer = stream;
    loop {
        match read_request(&mut reader) {
            Ok(Some(req)) => {
                captured.lock().unwrap().push(req);
                if writer
                    .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok")
                    .is_err()
                {
                    break;
                }
            }
            _ => break, // EOF or parse error: connection over
        }
    }
}

/// Spawn the test endpoint on an ephemeral port; returns the port.
/// Captured requests are appended to `captured`. The thread runs detached
/// until the test process exits.
fn spawn_endpoint(captured: Arc<Mutex<Vec<CapturedRequest>>>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    listener.set_nonblocking(true).unwrap();
    std::thread::spawn(move || {
        for _ in 0..6000 {
            // ~30s lifetime max
            match listener.accept() {
                Ok((stream, _)) => {
                    stream.set_nonblocking(false).unwrap();
                    let captured = Arc::clone(&captured);
                    std::thread::spawn(move || handle_conn(stream, &captured));
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

fn run_plumbrs(client: &str, port: u16, body: &str, requests: u32) {
    let bin = env!("CARGO_BIN_EXE_plumbrs");
    let mut child = std::process::Command::new(bin)
        .args([
            "-t",
            "1",
            "-c",
            "1",
            "-r",
            &requests.to_string(),
            "-M",
            "POST",
            "-b",
            body,
            "-C",
            client,
        ])
        .arg(format!("http://127.0.0.1:{port}/echo"))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();

    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        match child.try_wait().unwrap() {
            Some(status) => {
                assert!(status.success(), "{client} client exited with {status}");
                return;
            }
            None if Instant::now() > deadline => {
                child.kill().ok();
                panic!("{client} client timed out");
            }
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    }
}

fn assert_body_delivered(client: &str, body: &str, requests: usize) {
    let captured: Arc<Mutex<Vec<CapturedRequest>>> = Arc::new(Mutex::new(Vec::new()));
    let port = spawn_endpoint(Arc::clone(&captured));
    run_plumbrs(client, port, body, requests as u32);

    // The client may return just before the endpoint flushed its log.
    let deadline = Instant::now() + Duration::from_secs(10);
    while captured.lock().unwrap().len() < requests && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }

    let captured = captured.lock().unwrap();
    assert_eq!(
        captured.len(),
        requests,
        "{client} client: upstream saw {} requests, expected {requests}",
        captured.len()
    );
    for (i, req) in captured.iter().enumerate() {
        assert_eq!(
            req.content_length,
            Some(body.len()),
            "{client} client: request {i} missing content-length",
        );
        assert_eq!(
            req.body,
            body.as_bytes(),
            "{client} client: request {i} body not delivered upstream",
        );
    }
}

#[test]
fn hyper_transmits_body() {
    assert_body_delivered("hyper", "hello-body", 4);
}

#[cfg(feature = "compio")]
#[test]
fn compio_transmits_body() {
    assert_body_delivered("compio", "hello-body", 4);
}

#[cfg(all(target_os = "linux", feature = "monoio"))]
#[test]
fn monoio_transmits_body() {
    assert_body_delivered("monoio", "hello-body", 4);
}

#[cfg(all(target_os = "linux", feature = "tokio_uring"))]
#[test]
fn tokio_uring_transmits_body() {
    assert_body_delivered("tokio-uring", "hello-body", 4);
}
