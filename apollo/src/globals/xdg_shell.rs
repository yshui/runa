use std::{cell::RefCell, future::Future, rc::Rc};

use derivative::Derivative;
use hashbrown::HashMap;
use wl_common::InterfaceMessageDispatch;
use wl_protocol::stable::xdg_shell::{
    xdg_surface::v5 as xdg_surface, xdg_toplevel::v5 as xdg_toplevel,
    xdg_wm_base::v5 as xdg_wm_base,
};
use wl_server::{
    connection::{ClientContext, State},
    events::{DispatchTo, EventHandler},
    globals::{Bind, ConstInit},
};

use crate::shell::{xdg::Layout, HasShell};
#[derive(Debug)]
pub struct WmBase;

#[derive(InterfaceMessageDispatch, Derivative)]
#[derivative(Debug(bound = ""))]
pub enum WmBaseObject<Ctx>
where
    Ctx: ClientContext,
    Ctx::Context: HasShell,
{
    WmBase(crate::objects::xdg_shell::WmBase),
    Surface(crate::objects::xdg_shell::Surface<Ctx>),
    TopLevel(crate::objects::xdg_shell::TopLevel<Ctx>),
}

#[derive(Default)]
pub(crate) struct WmBaseState {
    pub(crate) pending_configure: Rc<RefCell<HashMap<u32, Layout>>>,
    // A buffer we swap with pending_configure when we need to use it, so there is something to
    // hold new surface ids.
    scratch_buffer:               Option<HashMap<u32, Layout>>,
}

impl<Ctx> EventHandler<Ctx> for WmBase
where
    Ctx: ClientContext + DispatchTo<Self> + State<WmBaseState>,
    Ctx::Context: HasShell,
    <Ctx::Context as HasShell>::Shell: crate::shell::xdg::XdgShell,
{
    type Error = std::io::Error;

    type Fut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a
        where
            Ctx: 'a;

    fn invoke(ctx: &mut Ctx) -> Self::Fut<'_> {
        use wl_server::{connection::Objects, objects::Object};
        async move {
            let state = ctx.state_mut().unwrap();
            let scratch_buffer = state.scratch_buffer.take().unwrap();
            let mut pending_configure = state.pending_configure.replace(scratch_buffer);
            // Avoid holding Ref across await
            for (surface, layout) in pending_configure.drain() {
                let surface = ctx.objects().borrow().get(surface).unwrap().clone();
                // Send role specific configure event
                let surface = match surface.cast::<crate::objects::xdg_shell::TopLevel<Ctx>>() {
                    Some(toplevel) => {
                        let role_object_id = toplevel
                            .0
                            .role::<crate::shell::xdg::TopLevel>()
                            .unwrap()
                            .object_id;
                        if let Some(size) = layout.extent {
                            ctx.send(role_object_id, xdg_toplevel::events::Configure {
                                height: size.h as i32,
                                width:  size.w as i32,
                                states: &[],
                            })
                            .await?;
                        }
                        toplevel.0.clone()
                    },
                    None => {
                        unimplemented!()
                    },
                };
                // Send xdg_surface.configure event
                let (serial, role_object_id) = {
                    let mut role = surface.role_mut::<crate::shell::xdg::Surface>().unwrap();
                    let serial = role.serial;
                    role.serial = role.serial.checked_add(1).unwrap_or(1.try_into().unwrap());
                    role.pending_serial.push_back(serial);
                    (serial, role.object_id)
                };
                ctx.send(role_object_id, xdg_surface::events::Configure {
                    serial: serial.get(),
                })
                .await?;
            }
            // retain allocated buffer
            let state = ctx.state_mut().unwrap();
            state.scratch_buffer = Some(pending_configure);
            Ok(())
        }
    }
}

type PinnedFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = T> + 'a>>;
impl ConstInit for WmBase {
    const INIT: Self = Self;
}
impl<Ctx: ClientContext> Bind<Ctx> for WmBase
where
    Ctx: State<WmBaseState>,
    Ctx::Context: HasShell,
    <Ctx::Context as HasShell>::Shell: crate::shell::xdg::XdgShell,
{
    type Objects = WmBaseObject<Ctx>;

    fn interface(&self) -> &'static str {
        xdg_wm_base::NAME
    }

    fn version(&self) -> u32 {
        xdg_wm_base::VERSION
    }

    fn bind<'a>(
        &'a self,
        client: &'a mut Ctx,
        _object_id: u32,
    ) -> PinnedFuture<'a, std::io::Result<Self::Objects>> {
        client.set_state(WmBaseState {
            pending_configure: Default::default(),
            scratch_buffer:    Some(Default::default()),
        });
        Box::pin(futures_util::future::ok(
            WmBaseObject::WmBase(crate::objects::xdg_shell::WmBase).into(),
        ))
    }
}
