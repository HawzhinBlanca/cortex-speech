extern crate tiny_http_fork;

use std::sync::Arc;
use std::thread;

fn main() {
    let server = Arc::new(tiny_http_fork::Server::http("0.0.0.0:9975").unwrap());
    println!("Now listening on port 9975");

    let mut handles = Vec::new();

    for _ in 0..4 {
        let server = server.clone();

        handles.push(thread::spawn(move || {
            for rq_res in server.incoming_requests() {
                if let Err(err) = rq_res {
                    println!("error: {}", err);
                    continue;
                }

                let response = tiny_http_fork::Response::from_string("hello world".to_string());
                let _ = rq.respond(response);
            }
        }));
    }

    for h in handles {
        h.join().unwrap();
    }
}
