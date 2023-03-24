use std::{
    cell::RefCell,
    rc::{Rc, Weak},
};

use apollo::{
    shell::{
        buffers,
        output::OutputChange,
        surface::{
            self, roles::subsurface_iter, KeyboardActivity, KeyboardState, PointerEventKind,
        },
        xdg::{Layout, XdgShell},
        Shell, ShellEvent,
    },
    utils::{
        geometry::{
            coords::{self, Map},
            Extent, Point, Rectangle, Scale,
        },
        WeakPtr,
    },
};
use derivative::Derivative;
use dlv_list::{Index, VecList};
use ordered_float::NotNan;
use slotmap::{DefaultKey, SlotMap};
use tinyvec::TinyVec;
use winit::event::MouseButton;
use wl_protocol::wayland::wl_pointer::v8 as wl_pointer;
use wl_server::events::{broadcast::Broadcast, EventSource};
use xkbcommon::xkb;

#[derive(Derivative)]
#[derivative(Debug(bound = ""))]
pub struct DefaultShell<S: buffers::BufferLike> {
    storage:          SlotMap<DefaultKey, (surface::SurfaceState<Self>, DefaultShellData)>,
    /// Window stack, from bottom to top
    stack:            VecList<Window>,
    pointer_position: Option<Point<NotNan<f32>, coords::Screen>>,
    /// Who did we send the last pointer event to?
    pointer_focus:    Option<Weak<surface::Surface<Self>>>,
    shell_event:      Broadcast<ShellEvent>,
    position_offset:  Point<i32, coords::Screen>,
    screen:           apollo::shell::output::Screen,
    #[derivative(Debug = "ignore")]
    keyboard_state:   xkb::State,
    keys:             TinyVec<[u8; 8]>,
}

impl<B: buffers::BufferLike> DefaultShell<B> {
    pub fn new(output: &Rc<apollo::shell::output::Output>, keymap: &xkb::Keymap) -> Self {
        Self {
            storage:          Default::default(),
            stack:            Default::default(),
            shell_event:      Default::default(),
            position_offset:  Point::new(1000, 1000),
            pointer_focus:    None,
            pointer_position: None,
            screen:           apollo::shell::output::Screen::new_single_output(output),
            keyboard_state:   xkb::State::new(keymap),
            keys:             Default::default(),
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

    /// Find the surface under the pointer, returns the surface token and
    /// pointer's position in that surface's coordinate system.
    pub fn surface_under_pointer(
        &mut self,
        position: Point<NotNan<f32>, coords::Screen>,
    ) -> Option<(
        Rc<surface::Surface<Self>>,
        Point<NotNan<f32>, coords::Surface>,
    )> {
        // TODO: handle per output scaling
        let scale = self.scale_f32().map(|f| NotNan::try_from(f).unwrap());
        tracing::trace!(?position, "pointer motion");
        let old_pointer_focus = self.pointer_focus.take();
        let mut found = None;
        'find: for top_level in self.stack.iter().rev() {
            let position = position.map(|p| {
                let window_position = top_level.position.to::<NotNan<f32>>();
                Point::new(p.x - window_position.x, window_position.y - p.y) / scale
            });
            tracing::trace!(
                "position: {position:?}, window position: {:?}",
                top_level.position
            );
            // TODO: handle surface.set_input_region
            // right now we use window geometry as a hack
            {
                let state = self.get(top_level.surface_state);
                let surface = state.surface().upgrade().unwrap();
                let role = surface.role::<apollo::shell::xdg::TopLevel>().unwrap();
                if let Some(geometry) = role.geometry() {
                    if !geometry.to().contains(position) {
                        continue
                    }
                }
            }
            for (surface_token, offset) in subsurface_iter(top_level.surface_state, self).rev() {
                // TODO: handle buffer transform
                let relative_position = position - offset.to::<NotNan<f32>>();
                let surface_state = self.get(surface_token);
                let Some(buffer) = surface_state.buffer() else { continue };
                let dimension = buffer
                    .dimension()
                    .to()
                    .map(|dim| dim / surface_state.buffer_scale_f32());
                if dimension.contains(relative_position) {
                    found = Some((self.get(surface_token).surface().clone(), relative_position));
                    break 'find
                }
            }
        }
        tracing::trace!("No surface under pointer");
        if let Some((surface, relative_position)) = found {
            let focus_changed = old_pointer_focus
                .as_ref()
                .map_or(true, |weak| !Weak::ptr_eq(weak, &surface));
            tracing::debug!("{focus_changed} {old_pointer_focus:?} {surface:?}");
            self.pointer_focus = Some(surface.clone());
            if focus_changed {
                self.send_keyboard_state();
            }
            Some((surface.upgrade().unwrap(), relative_position))
        } else {
            if let Some(weak_surface) = old_pointer_focus {
                if let Some(surface) = weak_surface.upgrade() {
                    // Nothing under the pointer, send a leave event to the last surface
                    // we sent a pointer event to.
                    surface.pointer_event(PointerEventKind::Leave);
                    surface.keyboard_event(KeyboardActivity::Leave);
                }
            }
            None
        }
    }

    fn send_keyboard_state(&self) {
        tracing::debug!("Sending keyboard state");
        if let Some(focus) = self.pointer_focus.as_ref().and_then(|s| s.upgrade()) {
            let state = KeyboardState {
                keys:                self.keys.clone(),
                locked_modifiers:    self.keyboard_state.serialize_mods(xkb::STATE_MODS_LOCKED),
                latched_modifiers:   self.keyboard_state.serialize_mods(xkb::STATE_MODS_LATCHED),
                depressed_modifiers: self
                    .keyboard_state
                    .serialize_mods(xkb::STATE_MODS_DEPRESSED),
                effective_layout:    self
                    .keyboard_state
                    .serialize_layout(xkb::STATE_LAYOUT_EFFECTIVE),
            };
            focus.keyboard_event(KeyboardActivity::Key(state));
        }
    }

    pub fn pointer_motion(&mut self, position: Point<NotNan<f32>, coords::Screen>) {
        self.pointer_position = Some(position);
        let Some((surface, relative_position)) = self.surface_under_pointer(position) else {
            return
        };
        tracing::trace!("Pointer event on surface {}", surface.object_id());
        surface.pointer_event(PointerEventKind::Motion {
            coords: relative_position,
        });
    }

    pub fn pointer_button(&mut self, button: MouseButton, pressed: bool) {
        let Some(surface) = self.pointer_focus.as_ref().and_then(|s| s.upgrade()) else {
            return
        };
        tracing::debug!(
            "Pointer event on surface {} {button:?} {pressed}",
            surface.object_id()
        );
        let button = match button {
            MouseButton::Left => input_event_codes::BTN_LEFT,
            MouseButton::Right => input_event_codes::BTN_RIGHT,
            MouseButton::Middle => input_event_codes::BTN_MIDDLE,
            MouseButton::Other(_) => return,
        };
        surface.pointer_event(PointerEventKind::Button {
            button,
            state: if pressed {
                wl_pointer::enums::ButtonState::Pressed
            } else {
                wl_pointer::enums::ButtonState::Released
            },
        });
    }

    pub fn key(&mut self, key: u8, pressed: bool) {
        if pressed {
            if self.keys.contains(&key) {
                tracing::warn!(
                    "Received pressed event for key {}, which was already pressed",
                    key
                );
                return
            }
            self.keys.push(key);
        } else {
            if !self.keys.contains(&key) {
                tracing::warn!(
                    "Received release event for key {}, which was not pressed",
                    key
                );
                return
            }
            self.keys.retain(|k| *k != key);
        }

        // update state
        // Offset the keycode by 8, as the evdev XKB rules reflect X's
        // broken keycode system, which starts at 8.
        self.keyboard_state.update_key(
            key as u32 + 8,
            if pressed {
                xkb::KeyDirection::Down
            } else {
                xkb::KeyDirection::Up
            },
        );
        self.send_keyboard_state();
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
                let buffer_scale_inverse = surface.current(self).buffer_scale_f32().inv();
                let geometry = geometry.to() * buffer_scale_inverse;
                let geometry = Extent::new(
                    geometry.w.into_inner() as i32,
                    geometry.h.into_inner() as i32,
                );
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
            if let Some(pointer_position) = self.pointer_position {
                // Re-calculate what's under the pointer and send it a enter event
                self.pointer_motion(pointer_position);
            }
        }
    }

    fn role_deactivated(&mut self, key: Self::Token, role: &'static str) {
        let (_, data) = self.storage.get_mut(key).unwrap();
        assert!(data.is_current);
        tracing::debug!("Deactivated role {:?} for {:?}", role, key);

        if role == "xdg_toplevel" {
            self.stack.remove(data.stack_index.unwrap()).unwrap();
            tracing::debug!("Removed {key:?} from stack: {:?}", self.stack);
            if let Some(pointer_position) = self.pointer_position {
                // Re-calculate what's under the pointer and send it a enter event
                self.pointer_motion(pointer_position);
            }
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

                self.storage.get_mut(new).unwrap().1.stack_index = Some(index);
                if let Some(pointer_position) = self.pointer_position {
                    // Re-calculate what's under the pointer and send it a enter event
                    self.pointer_motion(pointer_position);
                }
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
