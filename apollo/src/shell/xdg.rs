use std::{
    marker::PhantomData,
    num::{NonZeroU32, Wrapping},
};

use derivative::Derivative;
use wl_common::utils::geometry::{Extent, Logical, Rectangle};
use wl_protocol::stable::xdg_shell::{
    xdg_surface::v5 as xdg_surface, xdg_toplevel::v5 as xdg_toplevel,
};
use wl_server::provide_any::Demand;

use super::Shell;

/// xdg_surface role
#[derive(Debug)]
pub struct Surface {
    active:                      bool,
    geometry:                    Option<Rectangle<i32, Logical>>,
    pub(crate) pending_geometry: Option<Rectangle<i32, Logical>>,
    /// Pending configure events that haven't been ACK'd, associated with a
    /// oneshot channel which will be notified once the client ACK the
    /// configure event.
    pending_serial:              Vec<(u32, futures_channel::oneshot::Sender<()>)>,
    /// The serial in the last ack_configure request.
    last_ack:                    Option<NonZeroU32>,
    serial:                      Wrapping<NonZeroU32>,
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

    fn provide<'a>(&'a self, demand: &mut Demand<'a>) {
        demand.provide_ref(self);
    }

    fn provide_mut<'a>(&'a mut self, demand: &mut Demand<'a>) {
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

#[derive(Default, Debug)]
pub(crate) struct TopLevelState {
    pub(crate) min_size: Option<Extent<i32, Logical>>,
    pub(crate) max_size: Option<Extent<i32, Logical>>,
}

#[derive(Derivative)]
#[derivative(Debug(bound = ""), Default(bound = ""))]
pub struct TopLevel {
    pub(crate) base:    Surface,
    is_active:          bool,
    pub(crate) app_id:  Option<String>,
    pub(crate) title:   Option<String>,
    pub(crate) current: TopLevelState,
    pub(crate) pending: TopLevelState,
}

impl<S: Shell> super::surface::Role<S> for TopLevel {
    fn name(&self) -> &'static str {
        xdg_toplevel::NAME
    }

    fn deactivate(&mut self, shell: &mut S) {
        if !self.is_active {
            return
        }
        self.is_active = false;
    }

    fn is_active(&self) -> bool {
        self.is_active
    }

    fn provide<'a>(&'a self, demand: &mut Demand<'a>) {
        demand.provide_ref(self).provide_ref(&self.base);
    }

    fn provide_mut<'a>(&'a mut self, demand: &mut Demand<'a>) {
        if let Some(mut receiver) = demand.maybe_provide_mut() {
            receiver.provide(self);
        } else if let Some(mut receiver) = demand.maybe_provide_mut() {
            receiver.provide(&mut self.base);
        }
    }
}

struct Popup;
