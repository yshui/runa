//! These are traits that are typically implemented by the server context singleton, and related
//! types and traits.

pub trait Server {
    /// The per client context type.
    type Connection;
}

use crate::connection::InterfaceMeta;
pub trait Globals: Server {
    /// Bind the given global
    fn bind(&self, client: &Self::Connection, id: u32) -> Box<dyn InterfaceMeta>;
 
    /// Bind the registry. This is special because it is the only global that is not bound by its
    /// ID, instead it's bound via `wl_display.get_registry`.
    fn bind_registry(&self, client: &Self::Connection) -> Box<dyn InterfaceMeta>;
}
