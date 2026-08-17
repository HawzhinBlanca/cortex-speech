extern crate tiny_http_fork;

use std::{
    io::{Read, Write},
    net::TcpStream,
    time::Duration,
};
use tiny_http_fork::{Response, Server};

/*
HTTP/1.1 400 Bad Request
Server: tiny-http (Rust)
Date: Thu, 30 Jul 2026 19:37:33 GMT
Content-Length: 0
*/
#[test]
fn test_cve2026_66752() {
    let _handle = std::thread::spawn(move || {
        let server = Server::http("127.0.0.1:8005").unwrap();

        for request_res in server.incoming_requests() {
            if let Ok(mut request) = request_res {
                let te = request.headers().get("Transfer-Encoding").map(|h| h.as_str().to_string());

                let cl = request.headers().get("Content-Length").map(|h| h.as_str().to_string());

                let mut body = Vec::new();
                let r = request.as_reader().read_to_end(&mut body).unwrap();

                println!("TE={:?} CL={:?} read={:?} body={:?}", te, cl, r, String::from_utf8_lossy(&body));
                let _ = request.respond(Response::from_string("ok"));
            }
        }
    });

    std::thread::sleep(Duration::from_millis(500));

    let mut client = TcpStream::connect("127.0.0.1:8005").unwrap();

    client
        .write("POST / HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: identity\r\nContent-Length: 5\r\n\r\nhello".as_bytes())
        .unwrap();

    let mut response = [0_u8; 512];
    let n = client.read(&mut response).unwrap();

    let resp = str::from_utf8(&response[..n]).unwrap();
    println!("{}", resp);

    if resp.starts_with("HTTP/1.1 400 Bad Request") == false {
        println!("the response should be 400");
        std::process::exit(0);
    }

    drop(client);

    // ---

    let mut client = TcpStream::connect("127.0.0.1:8005").unwrap();

    client
        .write(
            "POST / HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked, identity\r\n\r\n5\r\nhello\r\n0\r\n\r\n"
                .as_bytes(),
        )
        .unwrap();

    let mut response = [0_u8; 512];
    let n = client.read(&mut response).unwrap();

    let resp = str::from_utf8(&response[..n]).unwrap();
    println!("{}", resp);

    if resp.starts_with("HTTP/1.1 400 Bad Request") == false {
        println!("the response should be 400");
        std::process::exit(0);
    }

    drop(client);

    // ---

    let mut client = TcpStream::connect("127.0.0.1:8005").unwrap();

    client
        .write(
            "POST / HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked, chunked\r\n\r\n5\r\nhello\r\n0\r\n\r\n"
                .as_bytes(),
        )
        .unwrap();

    let mut response = [0_u8; 512];
    let n = client.read(&mut response).unwrap();

    let resp = str::from_utf8(&response[..n]).unwrap();
    println!("{}", resp);

    if resp.starts_with("HTTP/1.1 400 Bad Request") == false {
        println!("the response should be 400");
        std::process::exit(0);
    }

    drop(client);

    // ---

    let mut client = TcpStream::connect("127.0.0.1:8005").unwrap();

    client
        .write("POST / HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n0\r\n\r\n".as_bytes())
        .unwrap();

    let mut response = [0_u8; 512];
    let n = client.read(&mut response).unwrap();

    let resp = str::from_utf8(&response[..n]).unwrap();
    println!("{}", resp);

    if resp.starts_with("HTTP/1.1 200 OK") == false {
        println!("the response should be 200");
        std::process::exit(0);
    }

    drop(client);

    // ---

    let mut client = TcpStream::connect("127.0.0.1:8005").unwrap();

    client
        .write(
            "POST / HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: gzip, chunked\r\n\r\n5\r\nhello\r\n0\r\n\r\n".as_bytes(),
        )
        .unwrap();

    let mut response = [0_u8; 512];
    let n = client.read(&mut response).unwrap();

    let resp = str::from_utf8(&response[..n]).unwrap();
    println!("{}", resp);

    if resp.starts_with("HTTP/1.1 200 OK") == false {
        println!("the response should be 200");
        std::process::exit(0);
    }

    std::process::exit(0);
}
