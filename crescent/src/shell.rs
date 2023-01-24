use std::{ffi::CString, rc::Rc};

use apollo::{
    shell::{
        buffers,
        output::OutputChange,
        surface::{self, roles::subsurface_iter},
        xdg::{Layout, XdgShell},
        Shell,
    },
    utils::{
        geometry::{coords, Extent, Point, Rectangle, Scale},
        WeakPtr,
    },
};
use derivative::Derivative;
use dlv_list::{Index, VecList};
use slotmap::{DefaultKey, SlotMap};

#[derive(Derivative)]
#[derivative(Debug(bound = ""))]
pub struct DefaultShell<S: buffers::Buffer> {
    storage:         SlotMap<DefaultKey, (surface::SurfaceState<Self>, DefaultShellData)>,
    stack:           VecList<Window>,
    listeners:       wl_server::events::Listeners,
    position_offset: Point<i32, coords::Screen>,
    screen:          apollo::shell::output::Screen,
}

impl<B: buffers::Buffer> DefaultShell<B> {
    pub fn new(output: &Rc<apollo::shell::output::Output>) -> Self {
        Self {
            storage:         Default::default(),
            stack:           Default::default(),
            listeners:       Default::default(),
            position_offset: Point::new(1000, 1000),
            screen:          apollo::shell::output::Screen::new_single_output(output),
        }
    }
}

#[derive(Debug)]
pub struct Window {
    pub surface_state: DefaultKey,
    /// Position of the top-left corner of the window.
    /// i.e. the window covers from (position.x, position.y - extent.height)
    /// to (position.x + extent.width, position.y)
    pub position:      Point<i32, coords::Screen>,
}

impl Window {
    pub fn normalized_position(
        &self,
        output: &apollo::shell::output::Output,
    ) -> Point<i32, coords::ScreenNormalized> {
        use apollo::utils::geometry::coords::Map as _;
        self.position
            .map(|p| (p.to() / output.scale_f32()).floor().to())
    }
}

#[derive(Default, Debug)]
pub struct DefaultShellData {
    pub is_current:  bool,
    pub stack_index: Option<Index<Window>>,
}
impl<B: buffers::Buffer> DefaultShell<B> {
    pub fn stack(&self) -> impl DoubleEndedIterator<Item = &Window> {
        self.stack.iter()
    }

    /// Notify the listeners that surfaces in this shell has been rendered.
    /// Should be called by your renderer implementation.
    pub fn notify_render(&self) {
        self.listeners.notify();
    }

    fn update_subtree_outputs(
        &mut self,
        root: DefaultKey,
        root_position: Point<i32, coords::ScreenNormalized>,
    ) {
        use apollo::utils::geometry::coords::Map as _;
        let output = self.screen.outputs.get(0).unwrap();
        // Recalculate surface overlaps
        for (surface, offset) in subsurface_iter(root, self) {
            tracing::debug!("Scanning surface {surface:?} for output updates");
            let state = self.get(surface).unwrap();
            let surface = state.surface();
            if let Some(buffer) = state.buffer() {
                tracing::debug!("Surface: {}, buffer: {}", surface.object_id(), buffer.object_id());
                let geometry = buffer.dimension();
                let buffer_scale_inverse =
                    Scale::uniform(1. / (surface.current(self).buffer_scale_f32()));
                let geometry = geometry.map(|g| (g.to() * buffer_scale_inverse).to()).to();
                let rectangle =
                    Rectangle::from_loc_and_size(offset.map(|o| o + root_position), geometry);
                tracing::debug!(?geometry, ?rectangle, "output: {:?}, overlaps: {}", output.logical_geometry(), output.overlaps(&rectangle));
                let old_outputs = surface.outputs();

                // If: (there are some outputs no longer overlapping with window anymore) ||
                // (there are some new outputs overlapping with window now)
                if old_outputs.iter().any(|output| {
                    !output
                        .upgrade()
                        .map(|output| output.overlaps(&rectangle))
                        .unwrap_or(true)
                }) || (output.overlaps(&rectangle) &&
                    !old_outputs.contains::<WeakPtr<_>>(&Rc::downgrade(output).into()))
                {
                    // Update the outputs
                    tracing::debug!("Updating outputs for surface {surface:?}");
                    drop(old_outputs);
                    let mut outputs = surface.outputs_mut();
                    outputs.retain(|output| {
                        output
                            .upgrade()
                            .map(|output| output.overlaps(&rectangle))
                            .unwrap_or(false)
                    });
                    if output.overlaps(&rectangle) {
                        outputs.insert(Rc::downgrade(output).into());
                    }
                    tracing::debug!("New outputs: {outputs:?}");
                    state.surface().notify_output_changed();
                }
            } else if !surface.outputs().is_empty() {
                tracing::debug!("Clearing outputs for surface {surface:?} because it has no buffer");
                surface.outputs_mut().clear();
                state.surface().notify_output_changed();
            }
        }
    }

    /// Change the size of the only output in this shell.
    pub fn update_size(&mut self, size: Extent<u32, coords::Screen>) {
        let output = self.screen.outputs.get_mut(0).unwrap().clone();
        if size != output.size() {
            tracing::debug!("Updating output size to {:?}", size);
            output.set_size(size);
            output.notify_change(OutputChange::GEOMETRY);
        }

        // Take the stack out to avoid borrowing self.
        let stack = std::mem::take(&mut self.stack);
        for window in stack.iter() {
            self.update_subtree_outputs(window.surface_state, window.normalized_position(&output))
        }
        // Put the stack back.
        let _ = std::mem::replace(&mut self.stack, stack);
    }

    pub fn scale_f32(&self) -> Scale<f32> {
        self.screen.outputs.get(0).unwrap().scale_f32()
    }
}
impl<B: buffers::Buffer> Shell for DefaultShell<B> {
    type Buffer = B;
    type Key = DefaultKey;

    #[tracing::instrument(skip_all)]
    fn allocate(&mut self, state: surface::SurfaceState<Self>) -> Self::Key {
        self.storage.insert((state, DefaultShellData::default()))
    }

    fn deallocate(&mut self, key: Self::Key) {
        tracing::debug!("Deallocating {:?}", key);
        let (_, data) = self.storage.remove(key).unwrap();
        if let Some(stack_index) = data.stack_index {
            self.stack.remove(stack_index);
        }
    }

    fn get(&self, key: Self::Key) -> Option<&surface::SurfaceState<Self>> {
        self.storage.get(key).map(|v| &v.0)
    }

    fn get_mut(&mut self, key: Self::Key) -> Option<&mut surface::SurfaceState<Self>> {
        self.storage.get_mut(key).map(|v| &mut v.0)
    }

    fn rotate(&mut self, to_key: Self::Key, from_key: Self::Key) {
        let [to, from] = self.storage.get_disjoint_mut([to_key, from_key]).unwrap();
        to.0.rotate_from(&from.0);
    }

    fn role_added(&mut self, key: Self::Key, role: &'static str) {
        let (_, data) = self.storage.get_mut(key).unwrap();
        assert!(data.is_current);

        if role == "xdg_toplevel" {
            let output = self.screen.outputs.get(0).unwrap();
            let position = self.position_offset;
            let window = Window {
                surface_state: key,
                position,
            };
            let normalized_position = window.normalized_position(output);
            self.position_offset += Point::new(100, 100);
            data.stack_index = Some(self.stack.push_back(window));
            tracing::debug!("Added to stack: {:?}", self.stack);

            self.update_subtree_outputs(key, normalized_position);
        }
    }

    fn post_commit(&mut self, old: Option<Self::Key>, new: Self::Key) {
        let (_, data) = &mut self.storage.get_mut(new).unwrap();
        let output = self.screen.outputs.get(0).unwrap();
        assert!(!data.is_current);
        data.is_current = true;
        if let Some(old) = old {
            let (_, old_data) = &mut self.storage.get_mut(old).unwrap();
            assert!(old_data.is_current);
            old_data.is_current = false;
            if let Some(index) = old_data.stack_index.take() {
                // The window is in the window stack, so we may need to update its outputs.
                let window = self.stack.get(index).unwrap();
                self.update_subtree_outputs(new, window.normalized_position(output));
                // If the old state is in the window stack, replace it with the new state.
                self.stack.get_mut(index).unwrap().surface_state = new;
                // Safety: we did the same thing above, with unwrap().
                unsafe { self.storage.get_mut(new).unwrap_unchecked() }
                    .1
                    .stack_index = Some(index);
            }
        }
    }

    fn add_render_listener(&self, listener: (wl_server::events::EventHandle, usize)) {
        self.listeners.add_listener(listener);
    }

    fn remove_render_listener(&self, listener: (wl_server::events::EventHandle, usize)) -> bool {
        self.listeners.remove_listener(listener)
    }
}

impl<B: apollo::shell::buffers::Buffer> XdgShell for super::DefaultShell<B> {
    fn layout(&self, key: Self::Key) -> Layout {
        Layout {
            position: None,
            extent:   Some(Extent::new(400, 300)),
        }
    }
}
