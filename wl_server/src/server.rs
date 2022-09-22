//! These are traits that are typically implemented by the server context
//! singleton, and related types and traits.

pub trait Server {
    /// The per client context type.
    type Connection;
}

use std::{marker::PhantomData, rc::Rc};

use crate::connection::InterfaceMeta;
pub trait Globals<S: Server> {
    /// Bind the given global
    fn bind(&self, client: &S::Connection, id: u32) -> Box<dyn InterfaceMeta>;

    /// Bind the registry. This is special because it is the only global that is
    /// not bound by its ID, instead it's bound via
    /// `wl_display.get_registry`.
    fn bind_registry(&self, client: &S::Connection) -> Box<dyn InterfaceMeta>;

    /// Add listener to get notified for registry events
    fn add_listener(&self, handle: &Rc<crate::events::EventFlags>);
}

pub trait Global<S: Server> {
    fn bind(&self, client: &S::Connection) -> Box<dyn InterfaceMeta>;
}

pub struct Registry<S: Server> {
    globals: Vec<Box<dyn Global<S>>>,
    _server: PhantomData<S>,
}

impl<S: Server> Globals<S> for Registry<S> {
    fn bind(&self, client: &<S as Server>::Connection, id: u32) -> Box<dyn InterfaceMeta> {
        todo!()
    }

    fn bind_registry(&self, client: &<S as Server>::Connection) -> Box<dyn InterfaceMeta> {
        todo!()
    }

    fn add_listener(&self, handle: &Rc<crate::events::EventFlags>) {
        todo!()
    }
}
