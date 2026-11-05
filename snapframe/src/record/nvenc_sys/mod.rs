//! Vendored Windows NVENC 12.1 ABI from nvidia-video-codec-sdk 0.4.0 (MIT).
//! Only the encoder bindings are included: no CUDA SDK, import libraries or
//! build-time binding generator is required. See LICENSE and NOTICE.
#![allow(warnings, clippy::all)]
pub mod api;
mod guid;
mod version;
