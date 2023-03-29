//! Implementation of various wayland protocols and interfaces.
//!
//! As a compositor writer, you can pick and choose implementations of wayland
//! objects and globals here to assemble your `Object` and `Global`. (See
//! [`runa_core::objects::AnyObject`] and [`runa_core::globals::AnyGlobal`] for
//! more information.) However, some of the objects or globals can have
//! dependencies on other objects or globals, meaning you must use those
//! together.
//!
//! Generally speaking, if you choose to use a global, you must also use the
//! objects it can create. For example, if you use
//! [`Compositor`](crate::globals::Compositor), you must also use
//! [`Surface`](crate::objects::compositor::Surface) and
//! [`Subsurface`](crate::objects::compositor::Subsurface).
//!
//! There are other dependencies, such as
//! [`Pointer`](crate::objects::input::Pointer) and
//! [`Keyboard`](crate::objects::input::Keyboard), which depends on our surface
//! implementation. Pay attention to the documentation of the objects and
//! globals you choose to use.

#![allow(incomplete_features)]
#![feature(type_alias_impl_trait, trait_upcasting)]
#![warn(
    missing_debug_implementations,
    missing_copy_implementations,
    missing_docs,
    rust_2018_idioms,
    single_use_lifetimes
)]

pub mod globals;
pub mod objects;
pub mod renderer_capability;
pub mod shell;
mod time;
pub mod utils;
