use ascii::{AsciiStr, AsciiString};
use std::borrow::{Borrow, Cow};
use std::cmp::Ordering;
use std::fmt::{self, Display, Formatter};
use std::hash::Hash;
use std::io::{self, Write};
use std::str::FromStr;

use unicase::Ascii;

/// Status code of a request or response.
#[derive(Eq, PartialEq, Copy, Clone, Debug, Ord, PartialOrd)]
pub struct StatusCode(pub u16);

impl StatusCode {
    pub const CONTINUE: StatusCode = StatusCode(100);
    pub const SWITCHING_PROTOCOLS: StatusCode = StatusCode(101);
    pub const PROCESSING: StatusCode = StatusCode(102);
    pub const EARLY_HINTS: StatusCode = StatusCode(103);

    pub const OK: StatusCode = StatusCode(200);
    pub const CREATED: StatusCode = StatusCode(201);
    pub const ACCEPTED: StatusCode = StatusCode(202);
    pub const NON_AUTHORITATIVE_INFORMATION: StatusCode = StatusCode(203);
    pub const NO_CONTENT: StatusCode = StatusCode(204);
    pub const RESET_CONTENT: StatusCode = StatusCode(205);
    pub const PARTIAL_CONTENT: StatusCode = StatusCode(206);
    pub const MULTI_STATUS: StatusCode = StatusCode(207);
    pub const ALREADY_REPORTED: StatusCode = StatusCode(208);
    pub const IMUSED: StatusCode = StatusCode(226);

    pub const MULTIPLE_CHOICES: StatusCode = StatusCode(300);
    pub const MOVED_PERMANENTLY: StatusCode = StatusCode(301);
    pub const FOUND: StatusCode = StatusCode(302);
    pub const SEE_OTHER: StatusCode = StatusCode(303);
    pub const NOT_MODIFIED: StatusCode = StatusCode(304);
    pub const USE_PROXY: StatusCode = StatusCode(305);
    pub const TEMPORARY_REDIRECT: StatusCode = StatusCode(307);
    pub const PERMANENT_REDIRECT: StatusCode = StatusCode(308);

    pub const BAD_REQUEST: StatusCode = StatusCode(400);
    pub const UNAUTHORIZED: StatusCode = StatusCode(401);
    pub const PAYMENT_REQUIRED: StatusCode = StatusCode(402);
    pub const FORBIDDEN: StatusCode = StatusCode(403);
    pub const NOT_FOUND: StatusCode = StatusCode(404);
    pub const METHOD_NOT_ALLOWED: StatusCode = StatusCode(405);
    pub const NOT_ACCEPTABLE: StatusCode = StatusCode(406);
    pub const PROXY_AUTHENTICATION_REQUIRED: StatusCode = StatusCode(407);
    pub const REQUEST_TIMEOUT: StatusCode = StatusCode(408);
    pub const CONFLICT: StatusCode = StatusCode(409);
    pub const GONE: StatusCode = StatusCode(410);
    pub const LENGTH_REQUIRED: StatusCode = StatusCode(411);
    pub const PRECONDITION_FAILED: StatusCode = StatusCode(412);
    pub const PAYLOAD_TOO_LARGE: StatusCode = StatusCode(413);
    pub const URITOO_LONG: StatusCode = StatusCode(414);
    pub const UNSUPPORTED_MEDIA_TYPE: StatusCode = StatusCode(415);
    pub const RANGE_NOT_SATISFIABLE: StatusCode = StatusCode(416);
    pub const EXPECTATION_FAILED: StatusCode = StatusCode(417);
    pub const MISDIRECTED_REQUEST: StatusCode = StatusCode(421);
    pub const UNPROCESSABLE_ENTITY: StatusCode = StatusCode(422);
    pub const LOCKED: StatusCode = StatusCode(423);
    pub const FAILED_DEPENDENCY: StatusCode = StatusCode(424);
    pub const UPGRADE_REQUIRED: StatusCode = StatusCode(426);
    pub const PRECONDITION_REQUIRED: StatusCode = StatusCode(428);
    pub const TOO_MANY_REQUESTS: StatusCode = StatusCode(429);
    pub const REQUEST_HEADER_FIELDS_TOO_LARGE: StatusCode = StatusCode(431);
    pub const UNAVAILABLE_FOR_LEGAL_REASONS: StatusCode = StatusCode(451);

    pub const INTERNAL_SERVER_ERROR: StatusCode = StatusCode(500);
    pub const NOT_IMPLEMENTED: StatusCode = StatusCode(501);
    pub const BAD_GATEWAY: StatusCode = StatusCode(502);
    pub const SERVICE_UNAVAILABLE: StatusCode = StatusCode(503);
    pub const GATEWAY_TIMEOUT: StatusCode = StatusCode(504);
    pub const HTTPVERSION_NOT_SUPPORTED: StatusCode = StatusCode(505);
    pub const VARIANT_ALSO_NEGOTIATES: StatusCode = StatusCode(506);
    pub const INSUFFICIENT_STORAGE: StatusCode = StatusCode(507);
    pub const LOOP_DETECTED: StatusCode = StatusCode(508);
    pub const NOT_EXTENDED: StatusCode = StatusCode(510);
    pub const NETWORK_AUTHENTICATION_REQUIRED: StatusCode = StatusCode(511);

    /// Returns the default reason phrase for this status code.
    /// For example the status code 404 corresponds to "Not Found".
    pub fn default_reason_phrase(&self) -> &'static str {
        match self.0 {
            100 => "Continue",
            101 => "Switching Protocols",
            102 => "Processing",
            103 => "Early Hints",

            200 => "OK",
            201 => "Created",
            202 => "Accepted",
            203 => "Non-Authoritative Information",
            204 => "No Content",
            205 => "Reset Content",
            206 => "Partial Content",
            207 => "Multi-Status",
            208 => "Already Reported",
            226 => "IM Used",

            300 => "Multiple Choices",
            301 => "Moved Permanently",
            302 => "Found",
            303 => "See Other",
            304 => "Not Modified",
            305 => "Use Proxy",
            307 => "Temporary Redirect",
            308 => "Permanent Redirect",

            400 => "Bad Request",
            401 => "Unauthorized",
            402 => "Payment Required",
            403 => "Forbidden",
            404 => "Not Found",
            405 => "Method Not Allowed",
            406 => "Not Acceptable",
            407 => "Proxy Authentication Required",
            408 => "Request Timeout",
            409 => "Conflict",
            410 => "Gone",
            411 => "Length Required",
            412 => "Precondition Failed",
            413 => "Payload Too Large",
            414 => "URI Too Long",
            415 => "Unsupported Media Type",
            416 => "Range Not Satisfiable",
            417 => "Expectation Failed",
            421 => "Misdirected Request",
            422 => "Unprocessable Entity",
            423 => "Locked",
            424 => "Failed Dependency",
            426 => "Upgrade Required",
            428 => "Precondition Required",
            429 => "Too Many Requests",
            431 => "Request Header Fields Too Large",
            451 => "Unavailable For Legal Reasons",

            500 => "Internal Server Error",
            501 => "Not Implemented",
            502 => "Bad Gateway",
            503 => "Service Unavailable",
            504 => "Gateway Timeout",
            505 => "HTTP Version Not Supported",
            506 => "Variant Also Negotiates",
            507 => "Insufficient Storage",
            508 => "Loop Detected",
            510 => "Not Extended",
            511 => "Network Authentication Required",
            _ => "Unknown",
        }
    }

    pub fn in_range_inc(&self, s: Self, e: Self) -> bool {
        (s.0..=e.0).contains(self)
    }
}

impl From<i8> for StatusCode {
    fn from(in_code: i8) -> StatusCode {
        StatusCode(in_code as u16)
    }
}

impl From<u8> for StatusCode {
    fn from(in_code: u8) -> StatusCode {
        StatusCode(in_code as u16)
    }
}

impl From<i16> for StatusCode {
    fn from(in_code: i16) -> StatusCode {
        StatusCode(in_code as u16)
    }
}

impl From<u16> for StatusCode {
    fn from(in_code: u16) -> StatusCode {
        StatusCode(in_code)
    }
}

impl From<i32> for StatusCode {
    fn from(in_code: i32) -> StatusCode {
        StatusCode(in_code as u16)
    }
}

impl From<u32> for StatusCode {
    fn from(in_code: u32) -> StatusCode {
        StatusCode(in_code as u16)
    }
}

impl AsRef<u16> for StatusCode {
    fn as_ref(&self) -> &u16 {
        &self.0
    }
}

impl PartialEq<u16> for StatusCode {
    fn eq(&self, other: &u16) -> bool {
        &self.0 == other
    }
}

impl PartialEq<StatusCode> for u16 {
    fn eq(&self, other: &StatusCode) -> bool {
        self == &other.0
    }
}

impl PartialOrd<u16> for StatusCode {
    fn partial_cmp(&self, other: &u16) -> Option<Ordering> {
        self.0.partial_cmp(other)
    }
}

impl PartialOrd<StatusCode> for u16 {
    fn partial_cmp(&self, other: &StatusCode) -> Option<Ordering> {
        self.partial_cmp(&other.0)
    }
}

/// Header parsing error.
#[derive(Debug)]
pub enum HeaderError {
    /// Protocol violation reason.
    ProtocolViolation(String),
}

impl fmt::Display for HeaderError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::ProtocolViolation(errmsg) => write!(f, "protocol violation: {}", errmsg),
        }
    }
}

#[derive(Debug)]
pub struct ConstHeader {
    field: &'static str,
    value: &'static str,
}

impl ConstHeader {
    const fn from_const(header: &'static str, value: &'static str) -> Self {
        Self { field: header, value: value }
    }
}

/// Connection: upgrade
pub const HEADER_CONNECTION: ConstHeader = ConstHeader::from_const("Connection", "upgrade");

/// Server: tiny-http (Rust)
pub const HEADER_SERVER: ConstHeader = ConstHeader::from_const("Server", "tiny-http (Rust)");

/// Transfer-Encoding: chunked
pub const HEADER_TRANSFER_ENCODING: ConstHeader = ConstHeader::from_const("Transfer-Encoding", "chunked");

/// Content-Type: text/plain; charset=UTF-8
pub const HEADER_CONTENT_TYPE: ConstHeader = ConstHeader::from_const("Content-Type", "text/plain; charset=UTF-8");

#[repr(transparent)]
#[derive(Debug, Clone)]
pub struct Headers(pub(crate) Vec<Header>);

impl From<Vec<Header>> for Headers {
    fn from(value: Vec<Header>) -> Self {
        Self(value)
    }
}

impl TryFrom<Vec<Cow<'static, str>>> for Headers {
    type Error = HeaderError;

    fn try_from(value: Vec<Cow<'static, str>>) -> Result<Self, Self::Error> {
        let headers = value.into_iter().map(|v| Header::try_from(v)).collect::<Result<Vec<Header>, Self::Error>>()?;

        return Ok(Self(headers));
    }
}

impl TryFrom<Vec<String>> for Headers {
    type Error = HeaderError;

    fn try_from(value: Vec<String>) -> Result<Self, Self::Error> {
        let headers = value.into_iter().map(|v| Header::try_from(v)).collect::<Result<Vec<Header>, Self::Error>>()?;

        return Ok(Self(headers));
    }
}

impl TryFrom<Vec<&str>> for Headers {
    type Error = HeaderError;

    fn try_from(value: Vec<&str>) -> Result<Self, Self::Error> {
        let headers = value.into_iter().map(|v| Header::try_from(v)).collect::<Result<Vec<Header>, Self::Error>>()?;

        return Ok(Self(headers));
    }
}

impl Headers {
    #[inline]
    pub fn new() -> Self {
        Self(Vec::new())
    }

    #[inline]
    pub fn with_capacity(cap: usize) -> Self {
        Self(Vec::with_capacity(cap))
    }

    #[inline]
    pub fn insert_kv(&mut self, key: &str, val: &str) -> Result<(), HeaderError> {
        let header = Header::from_str(key, val)?;

        self.0.push(header);

        return Ok(());
    }

    #[inline]
    pub fn insert(&mut self, idx: usize, key: &str, val: &str) -> Result<(), HeaderError> {
        let header = Header::from_str(key, val)?;

        self.0.insert(idx, header);

        return Ok(());
    }

    #[inline]
    pub fn insert_header(&mut self, idx: usize, hd: Header) {
        self.0.insert(idx, hd);
    }

    #[inline]
    pub fn push(&mut self, hd: Header) {
        self.0.push(hd);
    }

    #[cfg(not(feature = "allow_utf8_headers"))]
    #[inline]
    pub fn get(&self, key: &str) -> Option<&HeaderFieldValueAscii> {
        self.0.iter().find(|h: &&Header| h.field.equiv(key)).map(|h| &h.value)
    }

    #[cfg(feature = "allow_utf8_headers")]
    #[inline]
    pub fn get(&self, key: &str) -> Option<&HeaderFieldValueUtf> {
        self.0.iter().find(|h: &&Header| h.field.equiv(key)).map(|h| &h.value)
    }

    #[inline]
    pub fn get_header_mut(&mut self, key: &str) -> Option<&mut Header> {
        self.0.iter_mut().find(|h: &&mut Header| h.field.equiv(key))
    }

    #[inline]
    pub fn contains(&self, key: &str) -> bool {
        self.0.iter().any(|h| h.field == key)
    }

    pub(crate) fn write_headers<W: Write>(&self, writer: &mut W) -> io::Result<()> {
        for header in self.0.iter() {
            writer.write_all(header.field.as_str().as_ref())?;
            write!(writer, ": ")?;
            writer.write_all(header.value.as_str().as_ref())?;
            write!(writer, "\r\n")?;
        }

        return Ok(());
    }
}

/// Represents a HTTP header.
#[derive(Debug, Clone)]
pub struct Header {
    pub field: HeaderField,
    pub value: HeaderFieldValue,
}

impl Eq for Header {}

impl PartialEq for Header {
    fn eq(&self, other: &Self) -> bool {
        self.field == other.field
    }
}

impl PartialEq<str> for Header {
    fn eq(&self, other: &str) -> bool {
        self.field.equiv(other)
    }
}

/*impl Borrow<HeaderField> for Header
{
    fn borrow(&self) -> &HeaderField
    {
        &self.field
    }
}*/

impl Borrow<Ascii<AsciiString>> for Header {
    fn borrow(&self) -> &Ascii<AsciiString> {
        &self.field.0
    }
}

impl Hash for Header {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.field.hash(state);
    }
}

impl Header {
    fn from_const(ch: &ConstHeader) -> Self {
        let header = HeaderField::from_const(ch.field);
        let value = HeaderFieldValue::from_const(ch.value);

        return Header { field: header, value: value };
    }

    /// Builds a `Header` from two two `&[u8]`s.
    ///
    /// Example:
    ///
    /// ```ignore
    /// let header = tiny_http_fork::Header::from_bytes(&b"Content-Type"[..], &b"text/plain"[..]).unwrap();
    /// ```
    #[allow(clippy::result_unit_err)]
    pub fn from_bytes<B1, B2>(header: B1, value: B2) -> Result<Self, HeaderError>
    where
        B1: AsRef<[u8]>,
        B2: AsRef<[u8]>,
    {
        let header = HeaderField::try_from(header.as_ref())?;

        let value = HeaderFieldValue::from_slice(&header, value)?;

        return Ok(Header { field: header, value: value });
    }

    /// Builds a `Header` from two two `&str`s.
    ///
    /// Example:
    ///
    /// ```ignore
    /// let header = tiny_http_fork::Header::from_str("Content-Type", "text/plain").unwrap();
    /// ```
    pub fn from_str<S1, S2>(header: S1, value: S2) -> Result<Self, HeaderError>
    where
        S1: AsRef<str>,
        S2: AsRef<str>,
    {
        Self::from_bytes(header.as_ref(), value.as_ref())
    }
}

impl TryFrom<String> for Header {
    type Error = HeaderError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        return Self::try_from(value.as_str());
    }
}

impl TryFrom<&str> for Header {
    type Error = HeaderError;

    fn try_from(input: &str) -> Result<Self, Self::Error> {
        let mut elems = input.splitn(2, ':');
        let field = elems.next().ok_or(HeaderError::ProtocolViolation("no key val present".into()))?;
        let value = elems.next().ok_or(HeaderError::ProtocolViolation("no val val present".into()))?;

        return Self::from_bytes(field, value);
    }
}

impl From<&ConstHeader> for Header {
    fn from(value: &ConstHeader) -> Self {
        Self::from_const(value)
    }
}

impl TryFrom<Cow<'static, str>> for Header {
    type Error = HeaderError;

    fn try_from(value: Cow<'static, str>) -> Result<Self, Self::Error> {
        Self::try_from(value.as_ref())
    }
}

impl FromStr for Header {
    type Err = HeaderError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::try_from(s)
    }
}

impl Display for Header {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> Result<(), fmt::Error> {
        write!(formatter, "{}: {}", self.field, self.value.as_str())
    }
}

/// A [HeaderFieldValueAscii] which does not support UTF-8 in headers.
#[cfg(not(feature = "allow_utf8_headers"))]
pub type HeaderFieldValue = HeaderFieldValueAscii;

/// A [HeaderFieldValueUtf] which does not support UTF-8 in headers.
#[cfg(feature = "allow_utf8_headers")]
pub type HeaderFieldValue = HeaderFieldValueUtf;

#[cfg(feature = "allow_utf8_headers")]
pub mod mod_header_value_utf {
    use std::{
        collections::HashSet,
        sync::{LazyLock, OnceLock},
    };

    use super::*;

    const NON_UTF8_HEADERS: &'static [&'static str] = &[
        "A-IM",
        "Age",
        "Accept",
        "Accept-Charset",
        "Accept-Datetime",
        "Accept-Encoding",
        "Accept-Language",
        "Access-Control-Request-Method",
        "Access-Control-Allow-Origin",
        "Authorization",
        "Cache-Control",
        "Connection",
        "Content-Encoding",
        "Content-Length",
        "Content-MD5",
        "Content-Type",
        "Cookie",
        "Date",
        "ETag",
        "Expect",
        "Forwarded",
        "Host",
        "From",
        "HTTP2-Settings",
        "If-Match",
        "If-None-Match",
        "If-Range",
        "If-Unmodified-Since",
        "Max-Forwards",
        "Pragma",
        "Proxy-Authorization",
        "Referer",
        "Server",
        "Set-Cookie",
        "Transfer-Encoding",
        "User-Agent",
        "Upgrade",
        "X-Forwarded-Host",
        "X-Backend-Server",
        "X-Requested-With",
        "X-Forwarded-Proto",
        "X-HTTP-Method-Override",
        "X-Att-Deviceid",
        "X-Cache-Info",
        "Vary",
    ];

    static NON_UTF8_HEADERS_SET: LazyLock<HashSet<&'static str>> =
        LazyLock::new(|| NON_UTF8_HEADERS.iter().map(|s| *s).collect());

    #[derive(Debug, Clone, Eq, PartialEq)]
    pub struct HeaderFieldValueUtf(String);

    impl AsRef<str> for HeaderFieldValueUtf {
        fn as_ref(&self) -> &str {
            &self.0
        }
    }

    impl HeaderFieldValueUtf {
        pub(super) fn from_const(val: &'static str) -> Self {
            Self(val.to_string())
        }

        pub(crate) fn from_slice<V>(field: &HeaderField, value: V) -> Result<Self, HeaderError>
        where
            V: AsRef<[u8]>,
        {
            let val_str = str::from_utf8(value.as_ref()).map_err(|e| HeaderError::ProtocolViolation(e.to_string()))?;

            Self::from_str(field, val_str)
        }

        pub(crate) fn from_str(field: &HeaderField, value: &str) -> Result<Self, HeaderError> {
            let value_trimmed = value.trim();

            // reject values containing 0x0D, 0x0A or 0x00,
            // reject field names containing anything outside the RFC 9110 token set:
            //   Tokens are short textual identifiers that do not include whitespace or delimiters
            // Alphanumeric characters: U+0041 'A' ..= U+005A 'Z', or U+0061 'a' ..= U+007A 'z', or
            //    U+0030 '0' ..= U+0039 '9'.
            // OR
            // The following special characters: U+0021 ..= U+002F ! " # $ % & ' ( ) * + , - . /, or
            //    U+003A ..= U+0040 : ; < = > ? @, or U+005B ..= U+0060 [ \ ] ^ _ `, or
            //    U+007B ..= U+007E { | } ~

            // don't allow uncode in the base headers
            let res = if NON_UTF8_HEADERS_SET.contains(field.as_str().as_str()) == true {
                value_trimmed
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() == true || ch.is_ascii_punctuation() == true || ch == ' ')
            } else {
                value_trimmed
                    .chars()
                    .all(|ch| ch.is_alphanumeric() == true || ch.is_ascii_punctuation() == true || ch == ' ')
            };

            if res == false {
                return Err(HeaderError::ProtocolViolation(format!("header value contains invalid chars")));
            }

            return Ok(Self(value_trimmed.to_string()));
        }

        pub fn as_str(&self) -> &str {
            &self.0
        }
    }

    impl Display for HeaderFieldValueUtf {
        fn fmt(&self, formatter: &mut Formatter<'_>) -> Result<(), fmt::Error> {
            write!(formatter, "{}", self.0.as_str())
        }
    }
}

#[cfg(feature = "allow_utf8_headers")]
pub use self::mod_header_value_utf::HeaderFieldValueUtf;

#[cfg(not(feature = "allow_utf8_headers"))]
pub mod mod_header_value_ascii {
    use super::*;

    #[derive(Debug, Clone, Eq, PartialEq)]
    pub struct HeaderFieldValueAscii(AsciiString);

    impl AsRef<AsciiStr> for HeaderFieldValueAscii {
        fn as_ref(&self) -> &AsciiStr {
            &self.0
        }
    }

    impl HeaderFieldValueAscii {
        pub(super) fn from_const(key: &'static str) -> Self {
            Self(unsafe { AsciiString::from_ascii_unchecked(key) })
        }

        pub(crate) fn from_slice<V>(field: &HeaderField, value: V) -> Result<Self, HeaderError>
        where
            V: AsRef<[u8]>,
        {
            let val_str = AsciiStr::from_ascii(&value).map_err(|e| HeaderError::ProtocolViolation(e.to_string()))?;

            Self::from_str(field, val_str)
        }

        pub(crate) fn from_str(_field: &HeaderField, value: &AsciiStr) -> Result<Self, HeaderError> {
            let value_trimmed = value.trim();

            // reject values containing 0x0D, 0x0A or 0x00,
            // reject field names containing anything outside the RFC 9110 token set:
            //   Tokens are short textual identifiers that do not include whitespace or delimiters
            // Alphanumeric characters: U+0041 'A' ..= U+005A 'Z', or U+0061 'a' ..= U+007A 'z', or
            //    U+0030 '0' ..= U+0039 '9'.
            // OR
            // The following special characters: U+0021 ..= U+002F ! " # $ % & ' ( ) * + , - . /, or
            //    U+003A ..= U+0040 : ; < = > ? @, or U+005B ..= U+0060 [ \ ] ^ _ `, or
            //    U+007B ..= U+007E { | } ~
            if false
                == value_trimmed
                    .as_bytes()
                    .iter()
                    .all(|ch| ch.is_ascii_alphanumeric() == true || ch.is_ascii_punctuation() == true || *ch == b' ')
            {
                return Err(HeaderError::ProtocolViolation(format!("header value contains invalid chars")));
            }

            return Ok(Self(value_trimmed.to_ascii_string()));
        }

        pub fn as_str(&self) -> &str {
            self.0.as_str()
        }
    }

    impl Display for HeaderFieldValueAscii {
        fn fmt(&self, formatter: &mut Formatter<'_>) -> Result<(), fmt::Error> {
            write!(formatter, "{}", self.0.as_str())
        }
    }
}

#[cfg(not(feature = "allow_utf8_headers"))]
pub use self::mod_header_value_ascii::HeaderFieldValueAscii;

/// Field of a header (eg. `Content-Type`, `Content-Length`, etc.)
///
/// Comparison between two `HeaderField`s ignores case.
#[derive(Debug, Clone, Eq)]
pub struct HeaderField(Ascii<AsciiString>);

impl Hash for HeaderField {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.0.hash(state);
    }
}

impl HeaderField {
    fn from_const(key: &'static str) -> Self {
        Self(unsafe { Ascii::new(AsciiString::from_ascii_unchecked(key)) })
    }

    fn from_str(key: &AsciiStr) -> Result<Self, HeaderError> {
        // reject values containing 0x0D, 0x0A or 0x00,
        // reject field names containing anything outside the RFC 9110 token set:
        //   Tokens are short textual identifiers that do not include whitespace or delimiters
        //    US-ASCII visual characters not allowed in a token (DQUOTE and "(),/:;<=>?@[\]{}").
        // - Alphanumeric characters: U+0041 'A' ..= U+005A 'Z', or U+0061 'a' ..= U+007A 'z', or
        //    U+0030 '0' ..= U+0039 '9'. or - or _

        if false == key.as_str().chars().all(|ch| ch.is_ascii_alphanumeric() == true || ch == '-' || ch == '_') {
            return Err(HeaderError::ProtocolViolation("header key value contains invalid chars".into()));
        }

        return Ok(HeaderField(Ascii::new(key.to_ascii_string())));
    }

    pub fn as_str(&self) -> &AsciiStr {
        &self.0
    }

    pub fn equiv(&self, other: &str) -> bool {
        other.eq_ignore_ascii_case(self.as_str().as_str())
    }
}

impl TryFrom<Vec<u8>> for HeaderField {
    type Error = HeaderError;

    fn try_from(value: Vec<u8>) -> Result<Self, Self::Error> {
        let val_str = AsciiStr::from_ascii(&value).map_err(|e| HeaderError::ProtocolViolation(e.to_string()))?;

        return Self::from_str(val_str);
    }
}

impl TryFrom<&[u8]> for HeaderField {
    type Error = HeaderError;

    fn try_from(value: &[u8]) -> Result<Self, Self::Error> {
        let val_str = AsciiStr::from_ascii(value).map_err(|e| HeaderError::ProtocolViolation(e.to_string()))?;

        return Self::from_str(val_str);
    }
}

impl TryFrom<&str> for HeaderField {
    type Error = HeaderError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        let val_str = AsciiStr::from_ascii(value).map_err(|e| HeaderError::ProtocolViolation(e.to_string()))?;

        Self::from_str(val_str)
    }
}

impl TryFrom<String> for HeaderField {
    type Error = HeaderError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let val_str = AsciiStr::from_ascii(&value).map_err(|e| HeaderError::ProtocolViolation(e.to_string()))?;

        Self::from_str(val_str)
    }
}

impl FromStr for HeaderField {
    type Err = HeaderError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        HeaderField::try_from(s)
    }
}

impl Display for HeaderField {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> Result<(), fmt::Error> {
        write!(formatter, "{}", self.0.as_str())
    }
}

impl PartialEq for HeaderField {
    fn eq(&self, other: &HeaderField) -> bool {
        self.0 == other.0
    }
}

impl PartialEq<str> for HeaderField {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for HeaderField {
    fn eq(&self, other: &&str) -> bool {
        self.0 == other
    }
}

/// HTTP request methods
///
/// As per [RFC 7231](https://tools.ietf.org/html/rfc7231#section-4.1) and
/// [RFC 5789](https://tools.ietf.org/html/rfc5789)
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Method {
    /// `GET`
    Get,

    /// `HEAD`
    Head,

    /// `POST`
    Post,

    /// `PUT`
    Put,

    /// `DELETE`
    Delete,

    /// `CONNECT`
    Connect,

    /// `OPTIONS`
    Options,

    /// `TRACE`
    Trace,

    /// `PATCH`
    Patch,

    /// Request methods not standardized by the IETF
    NonStandard(AsciiString),
}

impl Method {
    pub fn as_str(&self) -> &str {
        match *self {
            Method::Get => "GET",
            Method::Head => "HEAD",
            Method::Post => "POST",
            Method::Put => "PUT",
            Method::Delete => "DELETE",
            Method::Connect => "CONNECT",
            Method::Options => "OPTIONS",
            Method::Trace => "TRACE",
            Method::Patch => "PATCH",
            Method::NonStandard(ref s) => s.as_str(),
        }
    }
}

impl FromStr for Method {
    type Err = ();

    fn from_str(s: &str) -> Result<Method, ()> {
        Ok(match s {
            "GET" => Method::Get,
            "HEAD" => Method::Head,
            "POST" => Method::Post,
            "PUT" => Method::Put,
            "DELETE" => Method::Delete,
            "CONNECT" => Method::Connect,
            "OPTIONS" => Method::Options,
            "TRACE" => Method::Trace,
            "PATCH" => Method::Patch,
            s => {
                let ascii_string = AsciiString::from_ascii(s).map_err(|_| ())?;
                Method::NonStandard(ascii_string)
            }
        })
    }
}

impl Display for Method {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> Result<(), fmt::Error> {
        write!(formatter, "{}", self.as_str())
    }
}

/// HTTP version (usually 1.0 or 1.1).
#[allow(clippy::upper_case_acronyms)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HTTPVersion(pub u8, pub u8);

impl Display for HTTPVersion {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> Result<(), fmt::Error> {
        write!(formatter, "{}.{}", self.0, self.1)
    }
}

impl Ord for HTTPVersion {
    fn cmp(&self, other: &Self) -> Ordering {
        let HTTPVersion(my_major, my_minor) = *self;
        let HTTPVersion(other_major, other_minor) = *other;

        if my_major != other_major {
            return my_major.cmp(&other_major);
        }

        my_minor.cmp(&other_minor)
    }
}

impl PartialOrd for HTTPVersion {
    fn partial_cmp(&self, other: &HTTPVersion) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq<(u8, u8)> for HTTPVersion {
    fn eq(&self, &(major, minor): &(u8, u8)) -> bool {
        self.eq(&HTTPVersion(major, minor))
    }
}

impl PartialEq<HTTPVersion> for (u8, u8) {
    fn eq(&self, other: &HTTPVersion) -> bool {
        let &(major, minor) = self;
        HTTPVersion(major, minor).eq(other)
    }
}

impl PartialOrd<(u8, u8)> for HTTPVersion {
    fn partial_cmp(&self, &(major, minor): &(u8, u8)) -> Option<Ordering> {
        self.partial_cmp(&HTTPVersion(major, minor))
    }
}

impl PartialOrd<HTTPVersion> for (u8, u8) {
    fn partial_cmp(&self, other: &HTTPVersion) -> Option<Ordering> {
        let &(major, minor) = self;
        HTTPVersion(major, minor).partial_cmp(other)
    }
}

impl From<(u8, u8)> for HTTPVersion {
    fn from((major, minor): (u8, u8)) -> HTTPVersion {
        HTTPVersion(major, minor)
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use httpdate::HttpDate;
    use std::time::{Duration, SystemTime};

    #[test]
    fn test_const_headers() {
        fn make_into(ch: &ConstHeader) {
            Header::from_bytes(ch.field, ch.value).unwrap();
        }

        make_into(&HEADER_CONNECTION);
        make_into(&HEADER_SERVER);
        make_into(&HEADER_TRANSFER_ENCODING);
        make_into(&HEADER_CONTENT_TYPE);
    }

    #[test]
    fn test_parse_header() {
        let header: Header = "Content-Type: text/html".parse().unwrap();

        assert!(header.field.equiv(&"content-type"));
        assert!(header.value.as_str() == "text/html");

        assert!("hello world".parse::<Header>().is_err());
    }

    #[test]
    fn formats_date_correctly() {
        let http_date = HttpDate::from(SystemTime::UNIX_EPOCH + Duration::from_secs(420895020));

        assert_eq!(http_date.to_string(), "Wed, 04 May 1983 11:17:00 GMT")
    }

    #[test]
    fn test_parse_header_with_doublecolon() {
        let header: Header = "Time: 20: 34".parse().unwrap();

        assert!(header.field.equiv(&"time"));
        assert!(header.value.as_str() == "20: 34");
    }

    // This tests reslstance to RUSTSEC-2020-0031: "HTTP Request smuggling
    // through malformed Transfer Encoding headers"
    // (https://rustsec.org/advisories/RUSTSEC-2020-0031.html).
    #[test]
    fn test_strict_headers() {
        assert!("Transfer-Encoding : chunked".parse::<Header>().is_err());
        assert!(" Transfer-Encoding: chunked".parse::<Header>().is_err());
        assert!("Transfer Encoding: chunked".parse::<Header>().is_err());
        assert!(" Transfer\tEncoding : chunked".parse::<Header>().is_err());
        assert!("Transfer-Encoding: chunked".parse::<Header>().is_ok());
        assert!("Transfer-Encoding: chunked ".parse::<Header>().is_ok());
        assert!("Transfer-Encoding:   chunked ".parse::<Header>().is_ok());
    }

    #[test]
    fn test_header_str() {
        Header::from_bytes("Content-Type", "text/plain; charset=UTF-8").unwrap();
        assert_eq!(Header::from_bytes("Content-Type", "text/plain; \x0acharset=UTF-8").is_err(), true);
        assert_eq!(Header::from_bytes("Content-Type", "text/plain; \x0dcharset=UTF-8").is_err(), true);
        assert_eq!(Header::from_bytes("Content-Type", "text/plain; charset=UTF-8\x00").is_err(), true);
        assert_eq!(Header::from_bytes("Content-Type", "text/plain; \x00charset=UTF-8").is_err(), true);
        assert_eq!(Header::from_bytes("Cont'ent-Type", "text/plain; charset=UTF-8").is_err(), true);
        assert_eq!(Header::from_bytes("Content@Type", "text/plain; charset=UTF-8").is_err(), true);
    }

    #[cfg(not(feature = "allow_utf8_headers"))]
    #[test]
    fn test_header_str_utf() {
        assert_eq!(Header::from_bytes("Auth-Custom-Type", "佳波").is_err(), true);
        assert_eq!(Header::from_bytes("Auth-Custom-Type", "Kanami").is_err(), false);
        assert_eq!(Header::from_bytes("Content-Type", "text/plain; charset=UTF-8; 由佳").is_err(), true);
    }

    #[cfg(feature = "allow_utf8_headers")]
    #[test]
    fn test_header_str_utf() {
        assert_eq!(Header::from_bytes("Auth-Custom-Type", "佳波").is_err(), false);
        assert_eq!(Header::from_bytes("Auth-Custom-Type", "Kanami").is_err(), false);
        assert_eq!(Header::from_bytes("Content-Type", "text/plain; charset=UTF-8; 由佳").is_err(), true);
    }

    #[test]
    fn test_hashset_header() {
        let mut hs = Headers::new();

        hs.insert_kv("Test", "test").unwrap();
        hs.insert_kv("test2", "test 2").unwrap();

        let val = hs.get("Test").unwrap();
        assert_eq!(val.as_str(), "test");

        let val = hs.get("test").unwrap();
        assert_eq!(val.as_str(), "test");

        let val = hs.get("test2").unwrap();
        assert_eq!(val.as_str(), "test 2");

        let val = hs.get("teSt2").unwrap();
        assert_eq!(val.as_str(), "test 2");

        let val = hs.get("TEST2").unwrap();
        assert_eq!(val.as_str(), "test 2");

        let val = hs.get("TEST3");
        assert_eq!(val.is_none(), true);
    }

    #[test]
    fn test_header_eq_test() {
        let h1 = Header::from_str("TestHeader", "TestValue").unwrap();
        let h2 = Header::from_str("anotherheader", "anotherheaderValue").unwrap();

        assert!(h1.field == "TestHeader");
        assert!(h1.field == "testHeader");
        assert!(h1.field == "testheader");
        assert!(h1.field == "testheadeR");
        assert!(h1.field != "testheade");
        assert!(h1.field != "test header");

        assert!(h2.field == "anotherheader");
        assert!(h2.field == "anotherHeader");
        assert!(h2.field == "Anotherheader");
        assert!(h2.field == "anOTherheader");
    }
}
