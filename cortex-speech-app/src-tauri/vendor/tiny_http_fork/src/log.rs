#[cfg(feature = "log")]
pub(crate) use log::{debug, error, info};

#[cfg(feature = "log_stdout")]
macro_rules! _debug {
    (target: $target:expr, $($arg:tt)+) => {println!($($arg)+)};
    ($($arg:tt)+) => {println!($($arg)+)};
}

#[cfg(feature = "log_stdout")]
macro_rules! _info {
    (target: $target:expr, $($arg:tt)+) => {println!($($arg)+)};
    ($($arg:tt)+) => {println!($($arg)+)};
}

#[cfg(feature = "log_stdout")]
macro_rules! _error {
    (target: $target:expr, $($arg:tt)+) => {println!($($arg)+)};
    ($($arg:tt)+) => {println!($($arg)+)};
}

#[cfg(feature = "log_stdout")]
#[allow(unused_imports)]
pub(crate) use {_debug as debug, _error as error, _info as info};

#[cfg(all(not(feature = "log"), not(feature = "log_stdout")))]
macro_rules! _debug {
    (target: $target:expr, $($arg:tt)+) => {};
    ($($arg:tt)+) => {};
}

#[cfg(all(not(feature = "log"), not(feature = "log_stdout")))]
macro_rules! _error {
    (target: $target:expr, $($arg:tt)+) => {};
    ($($arg:tt)+) => {};
}

#[cfg(all(not(feature = "log"), not(feature = "log_stdout")))]
macro_rules! _info {
    (target: $target:expr, $($arg:tt)+) => {};
    ($($arg:tt)+) => {};
}

#[cfg(all(not(feature = "log"), not(feature = "log_stdout")))]
#[allow(unused_imports)]
pub(crate) use {_debug as debug, _error as error, _info as info};
