use std::{collections::VecDeque, num::NonZeroU32};

use derivative::Derivative;
use wl_protocol::stable::xdg_shell::{
    xdg_surface::v5 as xdg_surface, xdg_toplevel::v5 as xdg_toplevel,
};
use wl_server::provide_any::Demand;

use super::Shell;
use crate::utils::geometry::{coords, Extent, Point, Rectangle};

#[derive(Debug, Default, Clone, Copy)]
pub struct Layout {
    pub position: Option<Point<i32, coords::Screen>>,
    pub extent:   Option<Extent<u32, coords::Screen>>,
}

pub trait XdgShell: Shell {
    fn layout(&self, key: Self::Token) -> Layout {
        Layout::default()
    }
}

/// xdg_surface role
#[derive(Debug, Clone)]
pub struct Surface {
    active:                      bool,
    geometry:                    Option<Rectangle<i32, coords::Surface>>,
    pub(crate) pending_geometry: Option<Rectangle<i32, coords::Surface>>,
    /// Pending configure events that haven't been ACK'd, associated with a
    /// oneshot channel which will be notified once the client ACK the
    /// configure event.
    pub(crate) pending_serial:   VecDeque<NonZeroU32>,
    /// The serial in the last ack_configure request.
    pub(crate) last_ack:         Option<NonZeroU32>,
    pub(crate) serial:           NonZeroU32,
    pub(crate) object_id:        u32,
}

impl Surface {
    #[inline]
    pub fn new(object_id: wl_types::NewId) -> Self {
        Self {
            active:           false,
            geometry:         None,
            pending_geometry: None,
            pending_serial:   VecDeque::new(),
            last_ack:         None,
            serial:           NonZeroU32::new(1).unwrap(),
            object_id:        object_id.0,
        }
    }

    fn commit<S: XdgShell>(
        &mut self,
        shell: &mut S,
        surface: &super::surface::Surface<S>,
        object_id: u32,
    ) -> Result<(), &'static str> {
        tracing::debug!("Committing xdg_surface");
        if surface.pending(shell).buffer().is_some() && self.last_ack.is_none() {
            return Err("Cannot attach buffer before the initial configure sequence is completed")
        }
        if self.pending_serial.is_empty() && self.last_ack.is_none() {
            // We haven't sent out the first configure event yet.
            // notify the configure listener which will send out the configure event.
            tracing::debug!("sending initial configure event");
            surface.notify_layout_changed(shell.layout(surface.current_key()));
        }
        self.geometry = self.pending_geometry;

        Ok(())
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

#[derive(Default, Debug, Clone, Copy)]
pub(crate) struct TopLevelState {
    pub(crate) min_size: Option<Extent<i32, coords::Surface>>,
    pub(crate) max_size: Option<Extent<i32, coords::Surface>>,
}

#[derive(Derivative)]
#[derivative(Debug(bound = ""))]
pub struct TopLevel {
    pub(crate) base:      Surface,
    is_active:            bool,
    pub(crate) app_id:    Option<String>,
    pub(crate) title:     Option<String>,
    pub(crate) current:   TopLevelState,
    pub(crate) pending:   TopLevelState,
    pub(crate) object_id: u32,
}

impl TopLevel {
    pub fn new(base: Surface, object_id: u32) -> Self {
        Self {
            base,
            is_active: true,
            app_id: None,
            title: None,
            current: TopLevelState::default(),
            pending: TopLevelState::default(),
            object_id,
        }
    }
}

impl<S: XdgShell> super::surface::Role<S> for TopLevel {
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

    fn pre_commit(
        &mut self,
        shell: &mut S,
        surface: &super::surface::Surface<S>,
    ) -> Result<(), &'static str> {
        tracing::debug!("Committing xdg_toplevel");
        let object_id = self.object_id;
        self.current = self.pending;

        self.base.commit(shell, surface, object_id)?;
        Ok(())
    }
}

struct Popup;
