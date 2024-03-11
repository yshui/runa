use runa_core::provide_any;

pub mod subsurface;
pub mod xdg;

pub use subsurface::Subsurface;
pub use xdg::Surface as XdgSurface;
pub use xdg::TopLevel as XdgTopLevel;
pub use xdg::Popup as XdgPopup;

/// A surface role
pub trait Role<S: crate::shell::Shell>: std::any::Any {
    /// The name of the interface of this role.
    fn name(&self) -> &'static str;
    /// Returns true if the role is active.
    ///
    /// As specified by the wayland protocol, a surface can be assigned a role,
    /// then have the role object destroyed. This makes the role "inactive",
    /// but the surface cannot be assigned a different role. So we keep the
    /// role object but "deactivate" it.
    fn is_active(&self) -> bool;
    /// Deactivate the role.
    fn deactivate(&mut self, shell: &mut S);
    /// Provides type based access to member variables of this role.
    fn provide<'a>(&'a self, _demand: &mut provide_any::Demand<'a>) {}
    /// Provides type based access to member variables of this role.
    fn provide_mut<'a>(&'a mut self, _demand: &mut provide_any::Demand<'a>) {}
    /// Called before the pending state becomes the current state, in
    /// [`Surface::commit`]. If an error is returned, the commit will be
    /// stopped.
    fn pre_commit(&mut self, _shell: &mut S, _surfacee: &super::Surface<S>) -> Result<(), &'static str> {
        Ok(())
    }
    /// Called after the pending state becomes the current state, in
    /// [`Surface::commit`]
    fn post_commit(&mut self, _shell: &mut S, _surface: &super::Surface<S>) {}
}

/// A double-buffer state associated with a role
pub trait State: std::any::Any + dyn_clone::DynClone + std::fmt::Debug + 'static {}

impl<S: crate::shell::Shell> provide_any::Provider for dyn Role<S> {
    fn provide<'a>(&'a self, demand: &mut provide_any::Demand<'a>) {
        self.provide(demand);
    }

    fn provide_mut<'a>(&'a mut self, demand: &mut provide_any::Demand<'a>) {
        self.provide_mut(demand);
    }
}
