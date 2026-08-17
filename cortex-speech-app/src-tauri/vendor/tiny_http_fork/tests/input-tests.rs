extern crate tiny_http_fork;

use std::io::{Read, Write};
use std::net::Shutdown;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

#[allow(dead_code)]
mod support;

#[test]
fn basic_string_input() {
    let (server, client) = support::new_one_server_one_client();

    {
        let mut client = client;
        (write!(client, "GET / HTTP/1.1\r\nHost: localhost\r\nContent-Type: text/plain; charset=utf8\r\nContent-Length: 5\r\n\r\nhello")).unwrap();
    }

    let mut request = server.recv().unwrap().unwrap();

    let mut output = String::new();
    request.as_reader().read_to_string(&mut output).unwrap();
    assert_eq!(output, "hello");
}

#[cfg(not(feature = "allow_utf8_headers"))]
#[test]
fn basic_string_input_utf8_not_allowed() {
    let (server, client) = support::new_one_server_one_client();

    {
        let mut client = client;
        (write!(client, "GET / HTTP/1.1\r\nHost: localhost\r\nContent-Type: text/plain; charset=utf8\r\nCustom-Header: 佳波\r\nContent-Length: 5\r\n\r\nhello")).unwrap();
    }

    let request = server.recv().unwrap();
    assert_eq!(request.is_err(), true);
}

#[cfg(feature = "allow_utf8_headers")]
#[test]
fn basic_string_input_utf8() {
    let (server, client) = support::new_one_server_one_client();

    {
        let mut client = client;
        (write!(client, "GET / HTTP/1.1\r\nHost: localhost\r\nContent-Type: text/plain; charset=utf8\r\nContent-Length: 5\r\nCustom-Header: 佳波\r\n\r\nhello")).unwrap();
    }

    let mut request = server.recv().unwrap().unwrap();

    let cust_header = request.headers().get("Custom-Header");
    assert_eq!(cust_header.is_some(), true);
    assert_eq!(cust_header.unwrap().as_str(), "佳波");

    let mut output = String::new();
    request.as_reader().read_to_string(&mut output).unwrap();
    assert_eq!(output, "hello");
}

#[test]
fn wrong_content_length() {
    let (server, client) = support::new_one_server_one_client();

    {
        let mut client = client;
        (write!(client, "GET / HTTP/1.1\r\nHost: localhost\r\nContent-Type: text/plain; charset=utf8\r\nContent-Length: 3\r\n\r\nhello")).unwrap();
    }

    let mut request = server.recv().unwrap().unwrap();

    let mut output = String::new();
    request.as_reader().read_to_string(&mut output).unwrap();
    assert_eq!(output, "hel");
}

#[test]
fn expect_100_continue() {
    let (server, client) = support::new_one_server_one_client();

    let mut client = client;
    (write!(client, "GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nExpect: 100-continue\r\nContent-Type: text/plain; charset=utf8\r\nContent-Length: 5\r\n\r\n")).unwrap();
    client.flush().unwrap();

    let (tx, rx) = mpsc::channel();

    thread::spawn(move || {
        let mut request = server.recv().unwrap().unwrap();
        let mut output = String::new();
        request.as_reader().read_to_string(&mut output).unwrap();
        assert_eq!(output, "hello");
        tx.send(()).unwrap();
    });

    // client.set_keepalive(Some(3)).unwrap(); FIXME: reenable this
    let mut content = vec![0; 12];
    client.read_exact(&mut content).unwrap();
    assert!(&content[9..].starts_with(b"100")); // 100 status code

    (write!(client, "hello")).unwrap();
    client.flush().unwrap();
    client.shutdown(Shutdown::Write).unwrap();

    rx.recv().unwrap();
}

#[test]
fn unsupported_expect_header() {
    let mut client = support::new_client_to_hello_world_server();

    (write!(client, "GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nExpect: 189-dummy\r\nContent-Type: text/plain; charset=utf8\r\n\r\n")).unwrap();

    // client.set_keepalive(Some(3)).unwrap(); FIXME: reenable this
    let mut content = String::new();
    client.read_to_string(&mut content).unwrap();
    assert!(&content[9..].starts_with("417")); // 417 status code
}

#[test]
fn invalid_header_name() {
    let mut client = support::new_client_to_hello_world_server();

    // note the space hidden in the Content-Length, which is invalid
    (write!(client, "GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Type: text/plain; charset=utf8\r\nContent-Length : 5\r\n\r\nhello")).unwrap();

    let mut content = String::new();
    client.read_to_string(&mut content).unwrap();
    assert!(&content[9..].starts_with("400 Bad Request")); // 400 status code
}

#[test]
fn oversized_header_line_is_rejected_without_reaching_the_application() {
    let (server, mut client) = support::new_one_server_one_client();
    let oversized = "a".repeat(9 * 1024);
    write!(client, "GET / HTTP/1.1\r\nHost: localhost\r\nX-Oversized: {oversized}\r\nConnection: close\r\n\r\n")
        .unwrap();

    let parsed = server.recv().unwrap();
    assert!(parsed.is_err(), "the oversized head must never become an application Request");

    let mut response = String::new();
    client.read_to_string(&mut response).unwrap();
    assert!(response.starts_with("HTTP/1.1 400"), "oversized head response: {response}");
}

#[test]
fn abandoned_declared_body_releases_the_request_worker_after_the_deadline() {
    let (server, mut client) = support::new_one_server_one_client();
    client.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    write!(client, "POST / HTTP/1.1\r\nHost: localhost\r\nContent-Length: 1000000\r\n\r\n").unwrap();
    client.flush().unwrap();

    let (done_tx, done_rx) = mpsc::channel();
    thread::spawn(move || {
        let request = server.recv().unwrap().unwrap();
        request.respond(tiny_http_fork::Response::empty(413)).unwrap();
        done_tx.send(()).unwrap();
    });

    // The response is prompt even though the peer deliberately sends no body.
    let mut response = [0_u8; 256];
    let read = client.read(&mut response).unwrap();
    assert!(
        String::from_utf8_lossy(&response[..read]).starts_with("HTTP/1.1 413"),
        "unexpected response: {}",
        String::from_utf8_lossy(&response[..read])
    );

    // More importantly, dropping Request no longer drains forever. Keep the client write side open
    // while waiting: only the library's absolute body deadline can release this worker.
    done_rx
        .recv_timeout(Duration::from_secs(15))
        .expect("an abandoned body must release its worker after the finite request deadline");
}

#[test]
fn custom_content_type_response_header() {
    let (server, mut stream) = support::new_one_server_one_client();
    write!(stream, "GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n").unwrap();

    let request = server.recv().unwrap().unwrap();

    request
        .respond(
            tiny_http_fork::Response::from_string("{\"custom\": \"Content-Type\"}")
                .with_header("Content-Type: application/json".parse::<tiny_http_fork::Header>().unwrap()),
        )
        .unwrap();

    let mut content = String::new();
    let n = stream.read_to_string(&mut content).unwrap();
    println!("{}", n);

    assert!(content.ends_with("{\"custom\": \"Content-Type\"}"));
    assert_ne!(content.find("Content-Type: application/json"), None);
}
