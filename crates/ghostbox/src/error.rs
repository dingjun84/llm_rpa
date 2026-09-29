//! Error types for GhostBox DLL loading, device open, and HID replay.

use std::fmt;

/// Errors from DLL load, FFI symbol lookup, device state, or replay.
#[derive(Debug, thiserror::Error)]
pub enum GhostboxError {
    #[error("failed to load GhostBox DLL: {0}")]
    Load(String),

    #[error("DLL symbol not found or call failed: {0}")]
    Symbol(String),

    #[error(
        "GhostBox device not connected (IsConnected={connected}, DeviceConnectState={state}); \
         refusing OpenDevice to avoid a hang when the device is missing"
    )]
    DeviceNotConnected { connected: i32, state: i32 },

    #[error("OpenDevice timed out after {timeout_ms} ms (device may be missing or SDK blocked)")]
    OpenDeviceTimeout { timeout_ms: u64 },

    #[error("OpenDevice returned {code}")]
    OpenDeviceFailed { code: i32 },

    #[error("SDK call `{op}` returned error code {code}")]
    Sdk { op: &'static str, code: i32 },

    #[error("replay sequence is empty")]
    EmptySequence,

    #[error("{0}")]
    Message(String),
}

impl GhostboxError {
    pub(crate) fn load(err: impl fmt::Display) -> Self {
        Self::Load(err.to_string())
    }

    pub(crate) fn symbol(err: impl fmt::Display) -> Self {
        Self::Symbol(err.to_string())
    }
}