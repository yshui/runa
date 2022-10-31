//! Render Capability, an interface for compositor implementation to tell us what its renderer can
//! do

pub use wl_protocol::wayland::wl_shm::v1::enums::Format;

pub trait RendererCapability {
    /// List of supported buffer pixel formats
    fn formats(&self) -> Vec<Format>;
}