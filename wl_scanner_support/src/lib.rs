#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

pub mod wayland_types {
    use fixed::types::extra::U8;
    pub struct NewId(pub u32);
    pub struct Fixed(pub fixed::FixedI32<U8>);
    pub struct Object(pub u32);
}

pub mod io {
    pub use wl_io::*;

    use pin_project_lite::pin_project;
    pin_project! {
    /// A convenience type to hold futures returned by the Deserializer trait.
    pub enum DeserializerFutureHolder<'de, D: Deserializer<'de>> {
        U32 {
            #[pin]
            inner: D::DeserializeU32
        },
        I32 {
            #[pin]
            inner: D::DeserializeI32
        },
        Bytes {
            #[pin]
            inner: D::DeserializeBytes
        },
        None,
    }
    }
}

pub use futures_lite::ready;
