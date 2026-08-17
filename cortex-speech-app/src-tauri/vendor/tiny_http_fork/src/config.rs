use std::{
    fs::File,
    io::{self, Read},
    net::ToSocketAddrs,
    path::Path,
};

use crate::connection::ConfigListenAddr;

/// Represents the parameters required to create a server.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// The addresses to try to listen to.
    pub addr: ConfigListenAddr,

    /// If `Some`, then the server will use SSL to encode the communications.
    pub ssl: Option<SslConfig>,

    /// Minimum threads which always runs.
    pub min_threads: Option<usize>,

    /// Maximum threads which can be allocated.
    pub max_threads: Option<usize>,
}

impl ServerConfig {
    pub fn new_http<A>(addr: A) -> io::Result<ServerConfig>
    where
        A: ToSocketAddrs,
    {
        Ok(ServerConfig {
            addr: ConfigListenAddr::from_socket_addrs(addr)?,
            ssl: None,
            min_threads: None,
            max_threads: None,
        })
    }

    pub fn set_min_threads(mut self, cnt: usize) -> Self {
        self.min_threads = Some(cnt);

        return self;
    }

    pub fn set_max_threads(mut self, cnt: usize) -> Self {
        self.max_threads = Some(cnt);

        return self;
    }

    pub fn set_ssl(mut self, ssl: SslConfig) -> Self {
        self.ssl = Some(ssl);

        return self;
    }
}

/// Configuration of the server for SSL.
#[derive(Debug, Clone)]
pub struct SslConfig {
    /// Contains the public certificate to send to clients.
    pub certificate: Vec<u8>,
    /// Contains the ultra-secret private key used to decode communications.
    pub private_key: Vec<u8>,
}

impl SslConfig {
    pub fn load_file(path: &Path) -> io::Result<Vec<u8>> {
        let mut cert_f = File::options().read(true).create(false).open(path)?;

        let file_size = cert_f.metadata()?.len();

        let mut file_buf = Vec::with_capacity(file_size as usize);
        cert_f.read_to_end(&mut file_buf)?;

        return Ok(file_buf);
    }

    pub fn load_from_fs<P: AsRef<Path>>(cert_path: P, key_path: P) -> io::Result<Self> {
        let ret = Self {
            certificate: Self::load_file(cert_path.as_ref())?,
            private_key: Self::load_file(key_path.as_ref())?,
        };

        return Ok(ret);
    }

    pub fn load_from_mem<CERT, KEY>(cert: CERT, key: KEY) -> Self
    where
        CERT: AsRef<[u8]> + Into<Vec<u8>>,
        KEY: AsRef<[u8]> + Into<Vec<u8>>,
    {
        Self { certificate: cert.into(), private_key: key.into() }
    }
}
