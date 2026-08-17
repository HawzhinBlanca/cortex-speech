//! Modules providing SSL/TLS implementations. For backwards compatibility, OpenSSL is the default
//! implementation, but Rustls is highly recommended as a pure Rust alternative.
//!
//! In order to simplify the swappable implementations these SSL/TLS modules adhere to an implicit
//! trait contract and specific implementations are re-exported as [`SslContextImpl`] and [`SslStream`].
//! The concrete type of these aliases will depend on which module you enable in `Cargo.toml`.

#[cfg(any(
    all(feature = "ssl-openssl", feature = "ssl-rustls"),
    all(feature = "ssl-openssl", feature = "ssl-native-tls"),
    all(feature = "ssl-native-tls", feature = "ssl-rustls"),
))]
compile_error!("Only one feature from 'ssl-openssl', 'ssl-rustls', 'ssl-native-tls' can be enabled at the same time");

#[cfg(feature = "ssl-openssl")]
pub(crate) mod openssl;
#[cfg(feature = "ssl-openssl")]
pub(crate) type SslContextImpl = openssl::OpenSslContext;
#[cfg(feature = "ssl-openssl")]
pub(crate) type SslStream = openssl::SplitOpenSslStream;

#[cfg(feature = "ssl-rustls")]
pub(crate) mod rustls;
#[cfg(feature = "ssl-rustls")]
pub(crate) type SslContextImpl = rustls::RustlsContext;
#[cfg(feature = "ssl-rustls")]
pub(crate) type SslStream = rustls::RustlsStream;

#[cfg(feature = "ssl-native-tls")]
pub(crate) mod native_tls;
#[cfg(feature = "ssl-native-tls")]
pub(crate) type SslContextImpl = native_tls::NativeTlsContext;
#[cfg(feature = "ssl-native-tls")]
pub(crate) type SslStream = native_tls::NativeTlsStream;
