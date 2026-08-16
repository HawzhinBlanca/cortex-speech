# tiny-http-fork

<img src="https://cdn.4neko.org/http-server.webp" width="200"/>

<br/>


<a href = "https://codeberg.org/4neko/tiny-http-fork/actions"><img src="https://codeberg.org/4neko/tiny-http-fork/badges/workflows/linux-os.yml/badge.svg" /></a>
<a href = "https://codeberg.org/4neko/tiny-http-fork/actions"><img src="https://codeberg.org/4neko/tiny-http-fork/badges/workflows/freebsd-os.yml/badge.svg" /></a>
<a href = "https://codeberg.org/4neko/tiny-http-fork/actions"><img src="https://codeberg.org/4neko/tiny-http-fork/badges/workflows/openbsd-os.yml/badge.svg" /></a>
<a href = "https://codeberg.org/4neko/tiny-http-fork/actions"><img src="https://codeberg.org/4neko/tiny-http-fork/badges/workflows/netbsd-os.yml/badge.svg" /></a>
<a href = "https://codeberg.org/4neko/tiny-http-fork/actions"><img src="https://codeberg.org/4neko/tiny-http-fork/badges/workflows/linux-os-arm.yml/badge.svg" /></a>

This is fork of original [tiny-http](https://github.com/tiny-http/tiny-http).

A new development repo: [codeberg](https://codeberg.org/4neko/tiny-http-fork)

Because original authors does not respond on CVE and this crate is the only which is lightweight and actually working, I (hopefully) temporarily decided to fork the crate and keep development and support there.

The LICENSES which were initially were preserved and published under the same licenses.

## Version
V 0.12.14-crate

## Release engineering

The current V0.12 is a next i.e 0.13 release. It is a collection of patches and improvements.

V0.13 will be created when code reaches stable.

## Features

### UTF-8
- `allow_utf8_headers` - enables the UTF-8 support in non-standart headers.

### Task pool
- `task_pool_legacy` - a legacy task pool (DEFAULT)
- `task_pool_channel` - a new task pool based on `crossbeam` and `nix`.

### Message queue
- `message_queue_legacy` - a legacy message queue (DEFAULT)
- `message_queue_channel` - a new implementation of queue based on crossbeam

## -- INFO/POLICY --

<details>
  <summary>AI (LLM) policy</summary>

- AI (LLM) generated sloppy code is prohibited. AI (LLM) generates slop "a priori" (anyway).
- It is strongly discouraged from using the AI based tools to write or enhance the code. AI slope would 100% violate the license by introducing the 3rd party licensed code. This code will never be accepted.
- It is ok to use the AI (LLM) for consultation purposes i.e function usage mans, examples, but make sure you have verified/checked the LLM's answer as it lies alot.

</details>  

## benching

```text
"task_pool_legacy", "message_queue_channel"

Timer precision: 9 ns
bench                         fastest       │ slowest       │ median        │ mean          │ samples │ iters
├─ header_parsing_with_ascii  99.79 ns      │ 5.939 µs      │ 109.7 ns      │ 173.4 ns      │ 100     │ 100
├─ parallel_requests          38.8 ms       │ 71.45 ms      │ 40.48 ms      │ 40.95 ms      │ 100     │ 100
╰─ sequential_requests        11.58 µs      │ 83.52 µs      │ 11.74 µs      │ 13.71 µs      │ 100     │ 100

"task_pool_legacy", "message_queue_legacy"

Timer precision: 10 ns
bench                         fastest       │ slowest       │ median        │ mean          │ samples │ iters
├─ header_parsing_with_ascii  89.8 ns       │ 3.009 µs      │ 99.8 ns       │ 130 ns        │ 100     │ 100
├─ parallel_requests          38.86 ms      │ 67.37 ms      │ 40.12 ms      │ 40.7 ms       │ 100     │ 100
╰─ sequential_requests        15.26 µs      │ 91.01 µs      │ 15.53 µs      │ 16.62 µs      │ 100     │ 100

"task_pool_channel", "message_queue_legacy"

Timer precision: 10 ns
bench                         fastest       │ slowest       │ median        │ mean          │ samples │ iters
├─ header_parsing_with_ascii  89.8 ns       │ 3.469 µs      │ 89.8 ns       │ 125.9 ns      │ 100     │ 100
├─ parallel_requests          38.72 ms      │ 71.06 ms      │ 40.03 ms      │ 41.4 ms       │ 100     │ 100
╰─ sequential_requests        14.21 µs      │ 42.73 µs      │ 14.41 µs      │ 15.17 µs      │ 100     │ 100

"task_pool_channel", "message_queue_channel"

Timer precision: 10 ns
bench                         fastest       │ slowest       │ median        │ mean          │ samples │ iters
├─ header_parsing_with_ascii  89.8 ns       │ 4.798 µs      │ 89.8 ns       │ 140.2 ns      │ 100     │ 100
├─ parallel_requests          38.77 ms      │ 76.46 ms      │ 39.7 ms       │ 40.27 ms      │ 100     │ 100
╰─ sequential_requests        11.55 µs      │ 46.2 µs       │ 11.85 µs      │ 13.92 µs      │ 100     │ 100
```

## ---- original readme ----

[**Documentation**](https://docs.rs/tiny_http_fork)

Tiny but strong HTTP server in Rust.
Its main objectives are to be 100% compliant with the HTTP standard and to provide an easy way to create an HTTP server.

What does **tiny-http** handle?
 - Accepting and managing connections to the clients
 - Parsing requests
 - Requests pipelining
 - HTTPS (using either OpenSSL, Rustls or native-tls)
 - Transfer-Encoding and Content-Encoding
 - Turning user input (eg. POST input) into a contiguous UTF-8 string (**not implemented yet**)
 - Ranges (**not implemented yet**)
 - `Connection: upgrade` (used by websockets)

Tiny-http handles everything that is related to client connections and data transfers and encoding.

Everything else (parsing the values of the headers, multipart data, routing, etags, cache-control, HTML templates, etc.) must be handled by your code.
If you want to create a website in Rust, I strongly recommend using a framework instead of this library.

### Installation

Add this to the `Cargo.toml` file of your project:

```toml
[dependencies]
tiny_http_fork = "0.11"
```

### Usage

```rust
use tiny_http_fork::{Server, Response};

let server = Server::http("0.0.0.0:8000").unwrap();

for request in server.incoming_requests() {
    println!("received request! method: {:?}, url: {:?}, headers: {:?}",
        request.method(),
        request.url(),
        request.headers()
    );

    let response = Response::from_string("hello world");
    request.respond(response);
}
```

### Speed

Tiny-http was designed with speed in mind:
 - Each client connection will be dispatched to a thread pool. Each thread will handle one client.
 If there is no thread available when a client connects, a new one is created. Threads that are idle
 for a long time (currently 5 seconds) will automatically die.
 - If multiple requests from the same client are being pipelined (ie. multiple requests
 are sent without waiting for the answer), tiny-http will read them all at once and they will
 all be available via `server.recv()`. Tiny-http will automatically rearrange the responses
 so that they are sent in the right order.
 - One exception to the previous statement exists when a request has a large body (currently > 1kB),
 in which case the request handler will read the body directly from the stream and tiny-http
 will wait for it to be read before processing the next request. Tiny-http will never wait for
 a request to be answered to read the next one.
 - When a client connection has sent its last request (by sending `Connection: close` header),
 the thread will immediately stop reading from this client and can be reclaimed, even when the
 request has not yet been answered. The reading part of the socket will also be immediately closed.
 - Decoding the client's request is done lazily. If you don't read the request's body, it will not
 be decoded.

### Examples

Examples of tiny-http in use:

* [heroku-tiny-http-hello-world](https://github.com/frewsxcv/heroku-tiny-http-hello-world) - A simple web application demonstrating how to deploy tiny-http to Heroku
* [crate-deps](https://github.com/frewsxcv/crate-deps) - A web service that generates images of dependency graphs for crates hosted on crates.io
* [rouille](https://crates.io/crates/rouille) - Web framework built on tiny-http

### License

This project is licensed under either of

 * Apache License, Version 2.0, ([LICENSE-APACHE](LICENSE-APACHE) or
   http://www.apache.org/licenses/LICENSE-2.0)
 * MIT license ([LICENSE-MIT](LICENSE-MIT) or
   http://opensource.org/licenses/MIT)

at your option.

#### Contribution

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in tiny-http by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.

<!-- Links and Badges -->
[crate_img]: https://img.shields.io/crates/v/tiny_http_fork.svg?logo=rust "Crate Page"
[crate]: https://crates.io/crates/tiny_http_fork "Crate Link"
[docs]: https://docs.rs/tiny_http_fork "Documentation"
[docs_img]: https://docs.rs/tiny_http_fork/badge.svg "Documentation"
[license_img]: https://img.shields.io/crates/l/tiny_http_fork.svg "License"
[ci_badge]: https://github.com/tiny-http/tiny-http/actions/workflows/ci.yaml/badge.svg "CI Status"
[ci_link]: https://github.com/tiny-http/tiny-http/actions/workflows/ci.yaml "Workflow Link"
