use crate::connection::Connection;
use crate::util::refined_tcp_stream::Stream as RefinedStream;
use rustls::pki_types::PrivateKeyDer;
use rustls::pki_types::pem::PemObject;
use std::error::Error;
use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr};
use std::sync::{Arc, Mutex};
use zeroize::Zeroizing;

/// A wrapper around an owned Rustls connection and corresponding stream.
///
/// Uses an internal Mutex to permit disparate reader & writer threads to access the stream independently.
pub(crate) struct RustlsStream(Arc<Mutex<rustls::StreamOwned<rustls::ServerConnection, Connection>>>);

impl RustlsStream {
    pub(crate) fn peer_addr(&mut self) -> std::io::Result<Option<SocketAddr>> {
        self.0.lock().expect("Failed to lock SSL stream mutex").sock.peer_addr()
    }

    pub(crate) fn shutdown(&mut self, how: Shutdown) -> std::io::Result<()> {
        self.0.lock().expect("Failed to lock SSL stream mutex").sock.shutdown(how)
    }
}

impl Clone for RustlsStream {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl Read for RustlsStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.0.lock().expect("Failed to lock SSL stream mutex").read(buf)
    }
}

impl Write for RustlsStream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("Failed to lock SSL stream mutex").write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.0.lock().expect("Failed to lock SSL stream mutex").flush()
    }
}

#[derive(Debug)]
pub(crate) struct RustlsContext(Arc<rustls::ServerConfig>);

impl RustlsContext {
    pub(crate) fn from_pem(
        certificates: Vec<u8>,
        private_key: Zeroizing<Vec<u8>>,
    ) -> Result<Self, Box<dyn Error + Send + Sync>> {
        let certificate_chain: Vec<rustls::pki_types::CertificateDer<'static>> =
            rustls_pemfile::certs(&mut certificates.as_slice())
                .into_iter()
                .collect::<Result<Vec<rustls::pki_types::CertificateDer<'static>>, std::io::Error>>()?;

        if certificate_chain.is_empty() {
            return Err("Couldn't extract certificate chain from config.".into());
        }

        let private_key = PrivateKeyDer::from_pem_slice(&private_key)?;

        let tls_conf =
            rustls::ServerConfig::builder().with_no_client_auth().with_single_cert(certificate_chain, private_key)?;

        Ok(Self(Arc::new(tls_conf)))
    }

    pub(crate) fn accept(&self, stream: Connection) -> Result<RustlsStream, Box<dyn Error + Send + Sync + 'static>> {
        let connection = rustls::ServerConnection::new(self.0.clone())?;

        Ok(RustlsStream(Arc::new(Mutex::new(rustls::StreamOwned::new(connection, stream)))))
    }
}

impl From<RustlsStream> for RefinedStream {
    fn from(stream: RustlsStream) -> Self {
        Self::Https(stream)
    }
}
