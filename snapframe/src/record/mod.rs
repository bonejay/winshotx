#[cfg(windows)]
pub(crate) mod audio;
#[cfg(windows)]
mod capture_session;
pub(crate) mod mezcla;
#[cfg(windows)]
pub(crate) mod nv12;
#[cfg(windows)]
pub(crate) mod nvenc;
#[cfg(windows)]
mod nvenc_sys;
pub(crate) mod stats;
pub(crate) mod stream_encoder;
#[cfg(windows)]
pub(crate) mod transport;
#[cfg(all(test, windows))]
pub(crate) mod validation;
#[cfg(windows)]
pub(crate) mod win;
