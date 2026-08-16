#![cfg(unix)]

extern crate tiny_http_fork;

use std::{
    io::{Read, Write},
    os::unix::net::UnixStream,
    path::PathBuf,
};

use tempfile::TempDir;

#[allow(dead_code)]
mod support;

#[test]
fn unix_basic_handling() {
    let tempdir = TempDir::new().unwrap();
    let p = tempdir.path().join("tiny-http-test.sock");
    let server = tiny_http_fork::Server::http_unix(&p).unwrap();
    let path: PathBuf = server.server_addr().to_unix().unwrap().as_pathname().unwrap().into();

    println!("serv path: {}", path.display());

    let mut client = UnixStream::connect(&path).unwrap();

    write!(client, "GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n").unwrap();

    let request = server.recv().unwrap().unwrap();
    assert!(*request.method() == tiny_http_fork::Method::Get);
    //assert!(request.url() == "/");
    request.respond(tiny_http_fork::Response::from_string("hello world".to_owned())).unwrap();

    server.try_recv().unwrap();

    let mut content = String::new();
    client.read_to_string(&mut content).unwrap();
    assert!(content.ends_with("hello world"));
}
