#![allow(clippy::too_many_arguments, clippy::module_inception)]
include!(concat!(env!("OUT_DIR"), "/wayland_generated.rs"));
pub mod stable {
    include!(concat!(env!("OUT_DIR"), "/stable_generated.rs"));
}
pub mod staging {
    include!(concat!(env!("OUT_DIR"), "/staging_generated.rs"));
}
pub mod unstable {
    include!(concat!(env!("OUT_DIR"), "/unstable_generated.rs"));
}
