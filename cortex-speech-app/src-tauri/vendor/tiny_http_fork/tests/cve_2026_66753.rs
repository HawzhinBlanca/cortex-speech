extern crate tiny_http_fork;

use std::{
    io::{Read, Write},
    net::TcpStream,
    time::Duration,
};
use tiny_http_fork::{Header, Response, Server};

#[test]
fn test_cve2026_66753() {
    let handle = std::thread::spawn(move || {
        let server = Server::http("127.0.0.1:8004").unwrap();

        for request_res in server.incoming_requests() {
            if let Ok(request) = request_res {
                if let Some(xtest) = request.headers().get("X-Test") {
                    println!("parsed X-Test value = {}", xtest.as_str());
                }

                let evil = "a\r\nX-Injected: yes";
                let mut resp = Response::from_string("body");
                resp.add_header(Header::from_str("X-Echo", evil).unwrap());
                let _ = request.respond(resp);
            }
        }
    });

    std::thread::sleep(Duration::from_millis(500));

    let mut client = TcpStream::connect("127.0.0.1:8004").unwrap();

    client.write("GET / HTTP/1.1\r\nHost: x\r\nX-Test: aaa\nbbb\r\nConnection: close\r\n\r\n".as_bytes()).unwrap();

    let mut response = [0_u8; 512];
    let n = client.read(&mut response).unwrap();

    let resp = str::from_utf8(&response[..n]).unwrap();
    println!("{}", resp);

    assert_eq!(resp.starts_with("HTTP/1.1 400 Bad Request"), true);

    drop(client);

    drop(handle);
}
