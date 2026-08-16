extern crate tiny_http_fork;

use divan::Bencher;
use std::io::Write;
use tiny_http_fork::{Header, Headers, Method, config::ServerConfig};

fn main() {
    // Run registered benchmarks.
    divan::main();
}

#[divan::bench]
fn header_test_vec(bencher: Bencher) {
    let mut hs = Headers::new();

    hs.insert_kv("Test", "test");
    hs.insert_kv("test2", "test 2");

    hs.insert_kv("Test2323", "test");
    hs.insert_kv("Test2323333", "test");
    hs.insert_kv("Test23233333", "test");

    hs.insert_kv("Test4323", "test");
    hs.insert_kv("Test5323333", "test");
    hs.insert_kv("Test63233333", "test");

    bencher.bench_local(move || {
        let val = hs.get("Test63233333").unwrap();
    });
}
