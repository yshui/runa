use std::{cell::RefCell, num::NonZeroU32, rc::Rc, collections::VecDeque};

use derivative::Derivative;
use hashbrown::HashMap;
use crate::utils::geometry::{Extent, Logical, Rectangle, Point};
use wl_protocol::stable::xdg_shell::{
    xdg_surface::v5 as xdg_surface, xdg_toplevel::v5 as xdg_toplevel,
};
use wl_server::{events::EventHandle, provide_any::Demand};

use super::Shell;

#[derive(Debug, Default, Clone, Copy)]
pub struct Layout {
    pub position: Option<Point<i32, Logical>>,
    pub extent: Option<Extent<u32, Logical>>,
}

pub trait XdgShell: Shell {
    fn layout(&self, key: Self::Key) -> Layout {
        Layout::default()
    }
}

fn surface_commit<S: XdgShell>(
    shell: &mut S,
    surface: &Rc<super::surface::Surface<S>>,
    role: &mut Surface,
    object_id: u32,
) -> Result<(), &'static str> {
    tracing::debug!("Committing xdg_surface");
    if surface.pending(shell).buffer().is_some() && role.last_ack.is_none() {
        return Err("Cannot attach buffer before the initial configure sequence is completed")
    }
    if role.pending_serial.is_empty() && role.last_ack.is_none() {
        // We haven't sent out the first configure event yet.
        // notify the configure listener which will send out the configure event.
        tracing::debug!("sending initial configure event");
        let alive = role
            .configure_listener
            .handle
            .set(role.configure_listener.slot);
        assert!(alive, "Surface not destructed after client disconnected");
        role.configure_listener
            .pending_configure
            .borrow_mut()
            .insert(object_id, shell.layout(surface.current_key()));
    }
    shell.commit(Some(surface.current_key()), surface.pending_key());
    shell.rotate(surface.current_key(), surface.pending_key());
    surface.swap_states();

    role.geometry = role.pending_geometry;
    Ok(())
}

fn toplevel_commit<S: XdgShell>(
    shell: &mut S,
    surface: &Rc<super::surface::Surface<S>>,
) -> Result<(), &'static str> {
    tracing::debug!("Committing xdg_toplevel");
    let mut role = surface.role_mut::<TopLevel>().unwrap();
    let object_id = role.object_id;
    surface_commit(shell, surface, &mut role.base, object_id)?;
    role.current = role.pending;
    Ok(())
}

#[derive(Debug, Clone)]
pub(crate) struct ConfigureListener {
    handle:            EventHandle,
    slot:              usize,
    pending_configure: Rc<RefCell<HashMap<u32, Layout>>>,
}

/// xdg_surface role
#[derive(Debug, Clone)]
pub struct Surface {
    active:                        bool,
    geometry:                      Option<Rectangle<i32, Logical>>,
    pub(crate) pending_geometry:   Option<Rectangle<i32, Logical>>,
    /// Pending configure events that haven't been ACK'd, associated with a
    /// oneshot channel which will be notified once the client ACK the
    /// configure event.
    pub(crate) pending_serial:     VecDeque<NonZeroU32>,
    /// The serial in the last ack_configure request.
    pub(crate) last_ack:           Option<NonZeroU32>,
    pub(crate) serial:             NonZeroU32,
    pub(crate) configure_listener: ConfigureListener,
    pub(crate) object_id:          u32,
}

impl Surface {
    #[inline]
    pub fn new(
        object_id: u32,
        listener: (EventHandle, usize),
        pending_list: &Rc<RefCell<HashMap<u32, Layout>>>,
    ) -> Self {
        Self {
            active:             false,
            geometry:           None,
            pending_geometry:   None,
            pending_serial:     VecDeque::new(),
            last_ack:           None,
            serial:             NonZeroU32::new(1).unwrap(),
            configure_listener: ConfigureListener {
                handle:            listener.0,
                slot:              listener.1,
                pending_configure: pending_list.clone(),
            },
            object_id,
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

#[derive(Default, Debug, Clone, Copy)]
pub(crate) struct TopLevelState {
    pub(crate) min_size: Option<Extent<i32, Logical>>,
    pub(crate) max_size: Option<Extent<i32, Logical>>,
}

#[derive(Derivative)]
#[derivative(Debug(bound = ""))]
pub struct TopLevel {
    pub(crate) base:    Surface,
    is_active:          bool,
    pub(crate) app_id:  Option<String>,
    pub(crate) title:   Option<String>,
    pub(crate) current: TopLevelState,
    pub(crate) pending: TopLevelState,
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

    fn commit_fn(
        &self,
    ) -> Option<fn(&mut S, &Rc<super::surface::Surface<S>>) -> Result<(), &'static str>> {
        tracing::debug!("commit_fn xdg_toplevel");
        Some(toplevel_commit::<S>)
    }
}

struct Popup;
