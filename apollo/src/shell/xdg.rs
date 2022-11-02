use std::{
    collections::HashMap,
    num::{NonZeroU32, Wrapping},
};

use wl_common::utils::geometry::{Logical, Rectangle};
use wl_protocol::stable::xdg_shell::xdg_surface::v5 as xdg_surface;

use super::Shell;

/// xdg_surface role
pub struct Surface {
    active:           bool,
    geometry:         Option<Rectangle<i32, Logical>>,
    pending_geometry: Option<Rectangle<i32, Logical>>,
    /// Pending configure events that haven't been ACK'd, associated with a
    /// oneshot channel which will be notified once the client ACK the
    /// configure event.
    pending_serial:   Vec<(u32, futures_channel::oneshot::Sender<()>)>,
    /// The serial in the last ack_configure request.
    last_ack:         Option<NonZeroU32>,
    serial:           Wrapping<NonZeroU32>,
}

impl Default for Surface {
    fn default() -> Self {
        Self {
            active:           false,
            geometry:         None,
            pending_geometry: None,
            pending_serial:   Vec::new(),
            last_ack:         None,
            serial:           Wrapping(NonZeroU32::new(1).unwrap()),
        }
    }
}

impl<S: Shell> super::surface::Role<S> for Surface {
    fn name(&self) -> &'static str {
        xdg_surface::NAME
    }

    fn provide<'a>(&'a self, demand: &mut wl_server::provide_any::Demand<'a>) {
        demand.provide_ref(self);
    }

    fn provide_mut<'a>(&'a mut self, demand: &mut wl_server::provide_any::Demand<'a>) {
        demand.provide_mut(self);
    }

    fn deactivate(&mut self, shell: &mut S) {
        if !self.active {
            return
        }
        self.active = false;
    }

    fn is_active(&self) -> bool {
        self.active
    }
}

struct TopLevel;
struct Popup;
