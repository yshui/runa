use std::{cell::RefCell, rc::Rc};

use apollo::{
    shell::{
        buffers,
        output::OutputChange,
        surface::{self, roles::subsurface_iter},
        xdg::{Layout, XdgShell},
        Shell, ShellEvent,
    },
    utils::{
        geometry::{coords, Extent, Point, Rectangle, Scale},
        WeakPtr,
    },
};
use derivative::Derivative;
use dlv_list::{Index, VecList};
use slotmap::{DefaultKey, SlotMap};
use wl_server::events::{broadcast::Broadcast, EventSource};

#[derive(Derivative)]
#[derivative(Debug(bound = ""))]
pub struct DefaultShell<S: buffers::BufferLike> {
    storage:         SlotMap<DefaultKey, (surface::SurfaceState<Self>, DefaultShellData)>,
    stack:           VecList<Window>,
    shell_event:     Broadcast<ShellEvent>,
    position_offset: Point<i32, coords::Screen>,
    screen:          apollo::shell::output::Screen,
}

impl<B: buffers::BufferLike> DefaultShell<B> {
    pub fn new(output: &Rc<apollo::shell::output::Output>) -> Self {
        Self {
            storage:         Default::default(),
            stack:           Default::default(),
            shell_event:     Default::default(),
            position_offset: Point::new(1000, 1000),
            screen:          apollo::shell::output::Screen::new_single_output(output),
        }
    }
}

#[derive(Debug)]
pub struct Window {
    pub(crate) surface_state: DefaultKey,
    /// Position of the top-left corner of the window.
    /// i.e. the window covers from (position.x, position.y - extent.height)
    /// to (position.x + extent.width, position.y)
    pub position:             Point<i32, coords::Screen>,
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
impl<B: buffers::BufferLike> DefaultShell<B> {
    pub fn stack(&self) -> impl DoubleEndedIterator<Item = &Window> {
        self.stack.iter()
    }

    /// Notify the listeners that surfaces in this shell has been rendered.
    /// Should be called by your renderer implementation.
    pub fn notify_render(&self) -> impl std::future::Future<Output = ()> {
        self.shell_event.broadcast_owned(ShellEvent::Render)
    }

    fn update_subtree_outputs(
        &self,
        root: DefaultKey,
        root_position: Point<i32, coords::ScreenNormalized>,
    ) {
        use apollo::utils::geometry::coords::Map as _;
        // Recalculate surface overlaps
        for (surface_token, offset) in subsurface_iter(root, self) {
            let state = self.get(surface_token);
            let surface = state.surface().upgrade().unwrap();
            tracing::debug!(
                "Scanning surface {surface_token:?} for output updates, surface id: {}",
                surface.object_id()
            );
            if let Some(buffer) = state.buffer() {
                tracing::debug!("Buffer: {}", buffer.object_id());
                let geometry = buffer.dimension();
                let buffer_scale_inverse =
                    Scale::uniform(1. / (surface.current(self).buffer_scale_f32()));
                let geometry = geometry.map(|g| (g.to() * buffer_scale_inverse).to()).to();
                // FIXME: offset is in Surface coordinates which is Y-down, while our output
                // coordinates are Y-up.
                let rectangle =
                    Rectangle::from_loc_and_size(offset.map(|o| o + root_position), geometry);
                let output = self.screen.outputs.get(0).unwrap();
                tracing::debug!(
                    ?geometry,
                    ?rectangle,
                    "output: {:?}, overlaps: {}",
                    output.logical_geometry(),
                    output.overlaps(&rectangle)
                );
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
                    state.surface().upgrade().unwrap().notify_output_changed();
                }
            } else if !surface.outputs().is_empty() {
                tracing::debug!(
                    "Clearing outputs for surface {surface:?} because it has no buffer"
                );
                surface.outputs_mut().clear();
                state.surface().upgrade().unwrap().notify_output_changed();
            }
        }
    }

    /// Change the size of the only output in this shell.
    pub async fn update_size(this: &RefCell<Self>, size: Extent<u32, coords::Screen>) {
        let output = this.borrow().screen.outputs.get(0).unwrap().clone();
        if size != output.size() {
            tracing::debug!("Updating output size to {:?}", size);
            output.set_size(size);
            output.notify_change(OutputChange::GEOMETRY).await;
        }

        // Take the stack out to avoid borrowing self.
        let mut this = this.borrow_mut();
        let stack = std::mem::take(&mut this.stack);
        for window in stack.iter() {
            this.update_subtree_outputs(window.surface_state, window.normalized_position(&output))
        }
        // Put the stack back.
        let _ = std::mem::replace(&mut this.stack, stack);
    }

    pub fn scale_f32(&self) -> Scale<f32> {
        self.screen.outputs.get(0).unwrap().scale_f32()
    }
}

impl<B: buffers::BufferLike> Shell for DefaultShell<B> {
    type Buffer = B;
    type Token = DefaultKey;

    #[tracing::instrument(skip_all)]
    fn allocate(&mut self, state: surface::SurfaceState<Self>) -> Self::Token {
        self.storage.insert((state, DefaultShellData::default()))
    }

    fn destroy(&mut self, key: Self::Token) {
        self.storage.remove(key).unwrap();
        tracing::debug!("Released {:?}, #surfaces left: {}", key, self.storage.len());
    }

    fn get(&self, key: Self::Token) -> &surface::SurfaceState<Self> {
        // This unwrap cannot fail, unless there is a bug in this implementation to
        // cause it to return an invalid token.
        self.storage
            .get(key)
            .map(|v| &v.0)
            .unwrap_or_else(|| panic!("Invalid token: {:?}", key))
    }

    fn get_mut(&mut self, key: Self::Token) -> &mut surface::SurfaceState<Self> {
        self.storage.get_mut(key).map(|v| &mut v.0).unwrap()
    }

    fn get_disjoint_mut<const N: usize>(
        &mut self,
        keys: [Self::Token; N],
    ) -> [&mut surface::SurfaceState<Self>; N] {
        self.storage
            .get_disjoint_mut(keys)
            .unwrap()
            .map(|v| &mut v.0)
    }

    fn role_added(&mut self, key: Self::Token, role: &'static str) {
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

    fn role_deactivated(&mut self, key: Self::Token, role: &'static str) {
        let (_, data) = self.storage.get_mut(key).unwrap();
        assert!(data.is_current);
        tracing::debug!("Deactivated role {:?} for {:?}", role, key);

        if role == "xdg_toplevel" {
            self.stack.remove(data.stack_index.unwrap()).unwrap();
            tracing::debug!("Removed {key:?} from stack: {:?}", self.stack);
        }
    }

    fn post_commit(&mut self, old: Option<Self::Token>, new: Self::Token) {
        tracing::debug!("post_commit: old: {old:?}, new: {new:?}");
        if old == Some(new) {
            // Nothing changed.
            return
        }

        let (_, data) = &mut self.storage.get_mut(new).unwrap();
        let output = self.screen.outputs.get(0).unwrap();
        assert!(
            !data.is_current,
            "a current state was committed to current again? {new:?}"
        );
        data.is_current = true;
        if let Some(old) = old {
            let (_, old_data) = &mut self.storage.get_mut(old).unwrap();
            assert!(old_data.is_current);
            old_data.is_current = false;
            // old_data might still be used, but if old_data.stack_index is Some(), then
            // it must be a root surface and thus the old data is no longer needed after
            // commit.
            if let Some(index) = old_data.stack_index.take() {
                // The window is in the window stack, so we may need to update its outputs.
                let window = self.stack.get(index).unwrap();
                self.update_subtree_outputs(new, window.normalized_position(output));
                // If the old state is in the window stack, replace it with the new state.
                let window = self.stack.get_mut(index).unwrap();
                window.surface_state = new;

                // Safety: we did the same thing above, with unwrap().
                unsafe { self.storage.get_mut(new).unwrap_unchecked() }
                    .1
                    .stack_index = Some(index);
            }
        }
    }
}

impl<S: buffers::BufferLike> EventSource<ShellEvent> for DefaultShell<S> {
    type Source = <Broadcast<ShellEvent> as EventSource<ShellEvent>>::Source;

    fn subscribe(&self) -> Self::Source {
        self.shell_event.subscribe()
    }
}

impl<B: buffers::BufferLike> XdgShell for super::DefaultShell<B> {
    fn layout(&self, _key: Self::Token) -> Layout {
        Layout {
            position: None,
            extent:   Some(Extent::new(400, 300)),
        }
    }
}
