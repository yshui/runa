//! Render capability.
//!
//! An interface for compositor implementation to tell us what its renderer can
//! do.

pub use runa_wayland_protocols::wayland::wl_shm::v1::enums::Format;

/// The renderer's capability
pub trait RendererCapability {
    /// List of supported buffer pixel formats
    fn formats(&self) -> Vec<Format>;
}
