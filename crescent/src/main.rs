#![feature(type_alias_impl_trait)]
use anyhow::Result;
use futures_util::TryStreamExt;
use std::cell::RefCell;
use std::os::unix::net::UnixStream;
use std::rc::Rc;
use wl_io::{buf::BufReaderWithFd, WithFd};
use wl_macros::message_broker;
use log::debug;

#[message_broker]
#[wayland(server_context = "Crescent")]
pub enum Messages {
    #[wayland(impl = "::wl_server::display::Display")]
    WlDisplay,
}

#[derive(Debug, Clone)]
pub struct Crescent {
    // We use thread-local executor and don't keep RefMut across await, so borrow_mut should never
    // fail.
    store: Rc<RefCell<wl_server::object_store::Store>>,
}

// Forward implementation of ObjectStore
impl wl_server::object_store::ObjectStore for Crescent {
    fn insert<T: wl_server::object_store::InterfaceMeta + 'static>(&mut self, id: u32, object: T) {
        self.store.borrow_mut().insert(id, object);
    }
    fn get(&self, id: u32) -> Option<Rc<dyn wl_server::object_store::InterfaceMeta>> {
        self.store.borrow().get(id)
    }
}

impl<'a> wl_server::AsyncContext<'a, UnixStream> for Crescent {
    type Error = anyhow::Error;
    type Task = impl std::future::Future<Output = Result<(), Self::Error>> + 'a;
    fn new_connection(&mut self, conn: UnixStream) -> Self::Task {
        debug!("New connection");
        use wl_server::object_store::ObjectStore;
        let this = self.clone();
        this.store.borrow_mut().insert(1, wl_server::display::Display);
        Box::pin(async move {
            let mut read = BufReaderWithFd::new(WithFd::try_from(conn)?);
            let _span = tracing::debug_span!("main loop").entered();
            while let (object_id, opcode, mut buf) =
                wl_io::de::WaylandBufAccess::next_message(&mut read).await?
            {
                debug!("Received message: object_id: {}, opcode: {}", object_id, opcode);
                Messages::dispatch(&this, object_id, &mut buf).await;
            }

            Ok(())
        })
    }
}

fn main() -> Result<()> {
    use futures_util::future;
    tracing_subscriber::fmt::init();
    let (listener, _guard) = wl_server::wayland_listener_auto()?;
    let listener = smol::Async::new(listener)?;
    let server = Crescent {
        store: Rc::new(RefCell::new(wl_server::object_store::Store::new())),
    };
    let executor = smol::LocalExecutor::new();
    let server = wl_server::AsyncServer::new(server, &executor);
    let incoming = Box::pin(listener.incoming().and_then(|conn| future::ready(conn.into_inner())));
    let mut cm = wl_server::ConnectionManager::new(
        incoming,
        server,
    );

    futures_executor::block_on(executor.run(cm.run()));

    Ok(())
}
