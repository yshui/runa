//! Type for representing output screens

use std::{
    cell::Cell,
    rc::{Rc, Weak},
};

use runa_core::events::{self, EventSource};
use runa_io::traits::WriteMessage;

use crate::utils::geometry::{coords, Extent, Point, Rectangle, Scale, Transform};

/// Output
///
/// An output is a screen, where the rendered result in displayed.
#[derive(Debug)]
pub struct Output {
    /// Resolution of the output, in pixels
    size:             Cell<Extent<u32, coords::Screen>>,
    /// Position of the output on the unified screen.
    position:         Cell<Point<i32, coords::Screen>>,
    /// Scale independent position of the output, used to calculate the
    /// of outputs and surfaces.
    logical_position: Cell<Point<i32, coords::ScreenNormalized>>,
    /// Physical size in millimeters
    physical_size:    Extent<u32, coords::Physical>,
    make:             String,
    model:            String,
    name:             String,
    transform:        Cell<Transform>,
    /// The scaling factor for this output, this is the numerator of a fraction
    /// with a denominator of 120.
    /// See the fractional_scale_v1::preferred_scale for why 120.
    scale:            Cell<Scale<u32>>,
    global_id:        u32,
    change_event:     events::broadcast::Broadcast<OutputChangeEvent>,
}

bitflags::bitflags! {
    /// Changes that can happen to an output
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    #[repr(transparent)]
    pub struct OutputChange: u32 {
        /// The output's geometry has changed
        const GEOMETRY = 1;
        /// The output's name has changed
        const NAME = 2;
        /// The output's scale has changed
        const SCALE = 4;
    }
}

impl Default for OutputChange {
    fn default() -> Self {
        Self::empty()
    }
}

impl std::hash::Hash for Output {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.global_id.hash(state);
    }
}

/// Event emitted when an output changes
#[derive(Clone, Debug)]
pub struct OutputChangeEvent {
    pub(crate) output: Weak<Output>,
    pub(crate) change: OutputChange,
}

impl Output {
    /// Checks whether an output overlaps a given rectangle.
    ///
    /// # Note
    ///
    /// This check uses a special coordinate system,
    /// [`coords::ScreenNormalized`](crate::utils::geometry::coords::ScreenNormalized),
    /// see the documentation there for the justification. If this is not what
    /// is the most suitable for your compositor, you are free implement
    /// other overlap checks. This is not used internally by this crate.
    pub fn overlaps(&self, other: &Rectangle<i32, coords::ScreenNormalized>) -> bool {
        self.logical_geometry().overlaps(other)
    }

    /// Notify that the output has changed
    pub async fn notify_change(self: &Rc<Self>, change: OutputChange) {
        self.change_event
            .broadcast(OutputChangeEvent {
                output: Rc::downgrade(self),
                change,
            })
            .await;
    }

    /// Create a new output
    pub fn new(
        physical_size: Extent<u32, coords::Physical>,
        make: &str,
        model: &str,
        name: &str,
        global_id: u32,
    ) -> Self {
        Self {
            size: Default::default(),
            position: Default::default(),
            logical_position: Default::default(),
            physical_size,
            make: make.to_owned(),
            model: model.to_owned(),
            name: name.to_owned(),
            transform: Default::default(),
            scale: Scale::new(1, 1).into(),
            global_id,
            change_event: Default::default(),
        }
    }

    /// The global ID in the compositor's global store.
    pub fn global_id(&self) -> u32 {
        self.global_id
    }

    /// Scale of the output
    pub fn scale(&self) -> Scale<u32> {
        self.scale.get()
    }

    /// Scale of the output as a f32
    pub fn scale_f32(&self) -> Scale<f32> {
        let scale = self.scale.get().to::<f32>();
        Scale::new(scale.x / 120., scale.y / 120.)
    }

    /// Set the scale of the output
    pub fn set_scale(&self, scale: u32) {
        self.scale.set(Scale::new(scale, scale));
    }

    /// Name of the output
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Scale invariant logical geometry of the output. This is used to
    /// determine where a window is placed, so that it is not dependent on
    /// the scale of the window. To avoid a dependency circle: window scale
    /// -> window placement -> window scale.
    ///
    /// Currently this is calculated as the size of the output divided by its
    /// scale.
    pub fn logical_geometry(&self) -> Rectangle<i32, coords::ScreenNormalized> {
        use crate::utils::geometry::coords::Map;
        let transform = self.transform.get();
        let scale = self.scale_f32();
        let logical_size = self
            .size
            .get()
            .map(|x| transform.transform_size(x.to() / scale).floor().to());
        let logical_position = self.logical_position.get();
        Rectangle::from_loc_and_size(logical_position, logical_size.to())
    }

    /// Geometry of the output in screen space.
    pub fn geometry(&self) -> Rectangle<i32, coords::Screen> {
        use crate::utils::geometry::coords::Map;
        let transform = self.transform.get();
        let size = self.size.get().map(|x| transform.transform_size(x));
        let position = self.position.get();
        Rectangle::from_loc_and_size(position, size.to())
    }

    /// Size of the output in screen space.
    pub fn size(&self) -> Extent<u32, coords::Screen> {
        self.size.get()
    }

    /// Position of the output in screen space.
    pub fn position(&self) -> Point<i32, coords::Screen> {
        self.position.get()
    }

    /// Scale invariant logical position of the output.
    ///
    /// See [`logical_geometry`](Self::logical_geometry).
    pub fn logical_position(&self) -> Point<i32, coords::ScreenNormalized> {
        self.logical_position.get()
    }

    /// Set the screen space size of the output
    pub fn set_size(&self, geometry: Extent<u32, coords::Screen>) {
        self.size.set(geometry);
    }

    /// Set the screen space position of the output
    pub fn set_position(&self, position: Point<i32, coords::Screen>) {
        self.position.set(position);
    }

    /// Set the scale invariant logical position of the output.
    ///
    /// See [`logical_geometry`](Self::logical_geometry).
    pub fn set_logical_position(&self, logical_position: Point<i32, coords::ScreenNormalized>) {
        self.logical_position.set(logical_position);
    }

    /// Model name of the output
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Make of the output
    pub fn make(&self) -> &str {
        &self.make
    }

    /// Output transformation
    pub fn transform(&self) -> Transform {
        self.transform.get()
    }

    /// Set the output transformation
    pub fn set_transform(&self, transform: Transform) {
        self.transform.set(transform);
    }

    /// Physical size in millimeters
    pub fn physical_size(&self) -> Extent<u32, coords::Physical> {
        self.physical_size
    }

    /// Send a wl_output.geometry event to the client
    pub(crate) async fn send_geometry<T: WriteMessage + Unpin>(
        &self,
        client: &mut T,
        object_id: u32,
    ) -> std::io::Result<()> {
        use runa_wayland_protocols::wayland::wl_output::v4 as wl_output;
        let geometry = self.geometry();
        let physical_size = self.physical_size();
        client
            .send(
                object_id,
                wl_output::Event::Geometry(wl_output::events::Geometry {
                    x:               geometry.loc.x,
                    y:               geometry.loc.y,
                    physical_width:  physical_size.w as i32,
                    physical_height: physical_size.h as i32,
                    subpixel:        wl_output::enums::Subpixel::Unknown, // TODO
                    make:            self.make().as_bytes().into(),
                    model:           self.model().as_bytes().into(),
                    transform:       wl_output::enums::Transform::Normal, // TODO
                }),
            )
            .await
    }

    /// Send a wl_output.name event to the client
    pub(crate) async fn send_name<T: WriteMessage + Unpin>(
        &self,
        client: &mut T,
        object_id: u32,
    ) -> std::io::Result<()> {
        use runa_wayland_protocols::wayland::wl_output::v4 as wl_output;
        client
            .send(
                object_id,
                wl_output::Event::Name(wl_output::events::Name {
                    name: self.name().as_bytes().into(),
                }),
            )
            .await
    }

    pub(crate) async fn send_scale<T: WriteMessage + Unpin>(
        &self,
        client: &mut T,
        object_id: u32,
    ) -> std::io::Result<()> {
        use runa_wayland_protocols::wayland::wl_output::v4 as wl_output;
        client
            .send(object_id, wl_output::events::Scale {
                factor: (self.scale.get().x / 120) as i32,
            })
            .await
    }

    /// Send a wl_output.done event to the client
    pub(crate) async fn send_done<T: WriteMessage + Unpin>(
        client: &mut T,
        object_id: u32,
    ) -> std::io::Result<()> {
        use runa_wayland_protocols::wayland::wl_output::v4 as wl_output;
        client
            .send(
                object_id,
                wl_output::Event::Done(wl_output::events::Done {}),
            )
            .await
    }

    /// Send all information about this output to the client.
    pub(crate) async fn send_all<T: WriteMessage + Unpin>(
        &self,
        client: &mut T,
        object_id: u32,
    ) -> std::io::Result<()> {
        self.send_geometry(client, object_id).await?;
        self.send_name(client, object_id).await?;
        self.send_scale(client, object_id).await?;
        Self::send_done(client, object_id).await
    }
}

impl EventSource<OutputChangeEvent> for Output {
    type Source =
        <events::broadcast::Broadcast<OutputChangeEvent> as EventSource<OutputChangeEvent>>::Source;

    fn subscribe(&self) -> Self::Source {
        self.change_event.subscribe()
    }
}

/// A collection of outputs
#[derive(Debug, Default)]
pub struct Screen {
    #[allow(missing_docs)]
    pub outputs: Vec<Rc<Output>>,
}

impl Screen {
    /// Find all outputs that overlaps with `geometry`
    pub fn find_outputs<'a>(
        &'a self,
        geometry: &'a Rectangle<i32, coords::ScreenNormalized>,
    ) -> impl Iterator<Item = &'a Output> {
        self.outputs
            .iter()
            .map(|o| o.as_ref())
            .filter(|output| output.overlaps(geometry))
    }

    /// Create a new screen with a single output
    pub fn from_single_output(output: &Rc<Output>) -> Self {
        Self {
            outputs: vec![output.clone()],
        }
    }
}
