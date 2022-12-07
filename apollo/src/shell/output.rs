use std::{
    cell::RefCell,
    ffi::{CStr, CString},
    rc::Rc,
};

use hashbrown::HashMap;
use wl_server::events::EventHandle;

use crate::utils::{
    geometry::{Extent, Logical, Physical, Rectangle},
    WeakPtr,
};

pub type OutputChanges = Rc<RefCell<HashMap<WeakPtr<Output>, OutputChange>>>;
#[derive(Debug, Eq, PartialEq)]
pub struct Output {
    geometry:      Rectangle<i32, Logical>,
    /// Physical size in millimeters
    physical_size: Extent<u32, Physical>,
    make:          CString,
    model:         CString,
    name:          CString,
    /// The scaling factor for this output, this is the numerator of a fraction with a denominator
    /// of 120.
    /// See the fractional_scale_v1::preferred_scale for why 120.
    scale:         u32,
    global_id:     u32,
    listeners:     RefCell<HashMap<(EventHandle, usize), OutputChanges>>,
}

bitflags::bitflags! {
    pub struct OutputChange: u32 {
        const GEOMETRY = 1;
        const NAME = 2;
        const SCALE = 4;
    }
}

#[allow(clippy::derive_hash_xor_eq)]
impl std::hash::Hash for Output {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.global_id.hash(state);
    }
}

impl Output {
    pub fn overlaps(&self, other: &Rectangle<i32, Logical>) -> bool {
        self.geometry.overlaps(other)
    }

    pub fn add_change_listener(&self, handle: (EventHandle, usize), changes_map: OutputChanges) {
        let old = self.listeners.borrow_mut().insert(handle, changes_map);
        assert!(old.is_none());
    }

    pub fn remove_change_listener(&self, handle: &(EventHandle, usize)) {
        self.listeners.borrow_mut().remove(handle);
    }

    pub fn new(
        geometry: Rectangle<i32, Logical>,
        physical_size: Extent<u32, Physical>,
        make: CString,
        model: CString,
        name: CString,
        scale: u32,
        global_id: u32,
    ) -> Self {
        Self {
            geometry,
            physical_size,
            make,
            model,
            name,
            scale,
            global_id,
            listeners: HashMap::new().into(),
        }
    }

    pub fn global_id(&self) -> u32 {
        self.global_id
    }

    pub fn scale(&self) -> u32 {
        self.scale
    }

    pub fn name(&self) -> &CStr {
        &self.name
    }

    pub fn geometry(&self) -> &Rectangle<i32, Logical> {
        &self.geometry
    }

    pub fn model(&self) -> &CStr {
        &self.model
    }

    pub fn make(&self) -> &CStr {
        &self.make
    }

    /// Physical size in millimeters
    pub fn physical_size(&self) -> Extent<u32, Physical> {
        self.physical_size
    }

    /// Send a wl_output::geometry event to the client
    pub(crate) async fn send_geometry<T: wl_server::connection::Client>(
        &self,
        client: &T,
        object_id: u32,
    ) -> std::io::Result<()> {
        use wl_protocol::wayland::wl_output::v4 as wl_output;
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
                    make:            wl_types::Str(self.make()),
                    model:           wl_types::Str(self.model()),
                    transform:       wl_output::enums::Transform::Normal, // TODO
                }),
            )
            .await
    }

    /// Send a wl_output::name event to the client
    pub(crate) async fn send_name<T: wl_server::connection::Client>(
        &self,
        client: &T,
        object_id: u32,
    ) -> std::io::Result<()> {
        use wl_protocol::wayland::wl_output::v4 as wl_output;
        client
            .send(
                object_id,
                wl_output::Event::Name(wl_output::events::Name {
                    name: wl_types::Str(self.name()),
                }),
            )
            .await
    }

    pub(crate) async fn send_scale<T: wl_server::connection::Client>(
        &self,
        client: &T,
        object_id: u32,
    ) -> std::io::Result<()> {
        use wl_protocol::wayland::wl_output::v4 as wl_output;
        client.send(object_id, wl_output::events::Scale {
            factor: (self.scale / 120) as i32,
        }).await
    }

    /// Send a wl_output::done event to the client
    pub(crate) async fn send_done<T: wl_server::connection::Client>(
        client: &T,
        object_id: u32,
    ) -> std::io::Result<()> {
        use wl_protocol::wayland::wl_output::v4 as wl_output;
        client
            .send(
                object_id,
                wl_output::Event::Done(wl_output::events::Done {}),
            )
            .await
    }

    /// Send all information about this output to the client.
    pub(crate) async fn send_all<T: wl_server::connection::Client>(
        &self,
        client: &T,
        object_id: u32,
    ) -> std::io::Result<()> {
        self.send_geometry(client, object_id).await?;
        self.send_name(client, object_id).await?;
        self.send_scale(client, object_id).await?;
        Self::send_done(client, object_id).await
    }
}

#[derive(Debug)]
pub struct Screen {
    outputs: Vec<Rc<Output>>,
}

impl Screen {
    /// Find all outputs that overlaps with `geometry`
    pub fn find_outputs<'a>(
        &'a self,
        geometry: &'a Rectangle<i32, Logical>,
    ) -> impl Iterator<Item = &'a Output> {
        self.outputs
            .iter()
            .map(|o| o.as_ref())
            .filter(|output| output.overlaps(geometry))
    }
}
