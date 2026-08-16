use std::fs;
use std::path::Path;

extern crate ascii;
extern crate tiny_http_fork;

fn get_content_type(path: &Path) -> &'static str {
    let extension = match path.extension() {
        None => return "text/plain",
        Some(e) => e,
    };

    match extension.to_str().unwrap() {
        "gif" => "image/gif",
        "jpg" => "image/jpeg",
        "jpeg" => "image/jpeg",
        "png" => "image/png",
        "pdf" => "application/pdf",
        "htm" => "text/html; charset=utf8",
        "html" => "text/html; charset=utf8",
        "txt" => "text/plain; charset=utf8",
        _ => "text/plain; charset=utf8",
    }
}

fn main() {
    let server = tiny_http_fork::Server::http("0.0.0.0:8000").unwrap();
    let port = server.server_addr().to_ip().unwrap().port();
    println!("Now listening on port {}", port);

    loop {
        let rq_req = match server.recv() {
            Ok(rq) => rq,
            Err(_) => break,
        };

        if let Err(err) = rq_req {
            println!("error: {}", err);
            continue;
        }

        let rq = rq_req.unwrap();

        println!("{:?}", rq);

        let url = rq.url().to_string();
        let path = Path::new(&url);
        let file = fs::File::open(&path);

        if file.is_ok() {
            let response = tiny_http_fork::Response::from_file(file.unwrap());

            let response = response
                .with_header(tiny_http_fork::Header::from_str("Content-Type", get_content_type(&path)).unwrap());

            let _ = rq.respond(response);
        } else {
            let rep = tiny_http_fork::Response::new_empty(tiny_http_fork::StatusCode(404));
            let _ = rq.respond(rep);
        }
    }
}
