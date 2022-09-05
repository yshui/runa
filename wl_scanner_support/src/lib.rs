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
}

pub use futures_lite::ready;
