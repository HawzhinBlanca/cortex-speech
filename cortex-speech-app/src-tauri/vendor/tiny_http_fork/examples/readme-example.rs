extern crate tiny_http_fork;

fn main() {
    use tiny_http_fork::{Response, Server};

    let server = Server::http("0.0.0.0:8000").unwrap();

    for request_res in server.incoming_requests() {
        if let Ok(request) = request_res {
            println!(
                "received request! method: {:?}, url: {:?}, headers: {:?}",
                request.method(),
                request.url(),
                request.headers()
            );

            let response = Response::from_string("hello world");
            request.respond(response).expect("Responded");
        } else {
            println!("received request with error: {}", request_res.err().unwrap())
        }
    }
}
