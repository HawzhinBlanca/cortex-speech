extern crate tiny_http_fork;

#[cfg(not(any(feature = "ssl-openssl", feature = "ssl-rustls", feature = "ssl-native-tls")))]
fn main() {
    println!("This example requires one of the supported `ssl-*` features to be enabled");
}

#[cfg(any(feature = "ssl-openssl", feature = "ssl-rustls", feature = "ssl-native-tls"))]
fn main() {
    use tiny_http_fork::{Response, Server};

    let server = Server::https(
        "0.0.0.0:8000",
        tiny_http_fork::SslConfig::load_from_mem(include_bytes!("ssl-cert.pem"), include_bytes!("ssl-key.pem")),
    )
    .unwrap();

    println!(
        "Note: connecting to this server will likely give you a warning from your browser \
              because the connection is unsecure. This is because the certificate used by this \
              example is self-signed. With a real certificate, you wouldn't get this warning."
    );

    for request in server.incoming_requests() {
        if let Err(err) = request {
            println!("error: {}", err);
            continue;
        }
        let request = request.unwrap();

        assert!(request.secure());

        println!(
            "received request! method: {:?}, url: {:?}, headers: {:?}",
            request.method(),
            request.url(),
            request.headers()
        );

        let response = Response::from_string("hello world");
        request.respond(response).unwrap_or(println!("Failed to respond to request"));
    }
}
