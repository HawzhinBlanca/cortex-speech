extern crate tiny_http_fork;

use divan::Bencher;
use std::io::Write;
use tiny_http_fork::{Header, Headers, Method, config::ServerConfig};

fn main() {
    // Run registered benchmarks.
    divan::main();
}

#[cfg(feature = "allow_utf8_headers")]
#[divan::bench]
fn header_parsing_with_utf8(bencher: Bencher) {
    bencher.bench_local(move || {
        Header::from_bytes("Auth-Custom-Type", "佳波").unwrap();
        Header::from_bytes("Auth-Custom-Type", "Kanami").unwrap();
        Header::from_bytes(
            "Other-Header",
            "指ずク熱体ばぴつこ真下せ長芸ゆぐも称断イへ超市へん覧追テ境査ぶる出研シヘメ野朝",
        )
        .unwrap();
    });
}

#[cfg(not(feature = "allow_utf8_headers"))]
#[divan::bench]
fn header_parsing_with_ascii(bencher: Bencher) {
    bencher.bench_local(move || {
        Header::from_bytes("Auth-Custom-Type", "Kanami").unwrap();
        Header::from_bytes("Other-Header", "very long text for the header data").unwrap();
    });
}

#[divan::bench]
fn sequential_requests(bencher: Bencher) {
    let cfg = ServerConfig::new_http("127.0.0.1:0").unwrap().set_min_threads(4).set_max_threads(64);

    let server = tiny_http_fork::Server::new(cfg).unwrap();
    let port = server.server_addr().to_ip().unwrap().port();

    let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();

    bencher.bench_local(move || {
        write!(stream, "GET / HTTP/1.1\r\nHost: localhost\r\n\r\n").unwrap();

        let request = server.recv().unwrap().unwrap();

        assert_eq!(request.method(), &Method::Get);

        request.respond(tiny_http_fork::Response::new_empty(tiny_http_fork::StatusCode(204))).unwrap();
    });
}

#[divan::bench]
fn parallel_requests(bencher: Bencher) {
    fdlimit::raise_fd_limit().unwrap();

    let cfg = ServerConfig::new_http("127.0.0.1:0").unwrap().set_min_threads(4).set_max_threads(64);

    let server = tiny_http_fork::Server::new(cfg).unwrap();
    let port = server.server_addr().to_ip().unwrap().port();

    bencher.bench_local(move || {
        let mut streams = Vec::new();

        for _ in 0..1000usize {
            let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
            (write!(stream, "GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")).unwrap();
            streams.push(stream);
        }

        loop {
            let request = match server.try_recv().unwrap() {
                None => break,
                Some(rq) => rq,
            };

            assert_eq!(request.method(), &Method::Get);

            request.respond(tiny_http_fork::Response::new_empty(tiny_http_fork::StatusCode(204))).unwrap();
        }
    });
}
