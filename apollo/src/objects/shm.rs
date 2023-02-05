use std::{
    cell::{Cell, Ref, RefCell},
    future::Future,
    os::{fd::AsRawFd, unix::io::OwnedFd},
    rc::Rc,
};

use dlv_list::{Index, VecList};
use spin::mutex::Mutex;
use wl_protocol::wayland::{
    wl_display::v1 as wl_display, wl_shm::v1 as wl_shm, wl_shm_pool::v1 as wl_shm_pool,
};
use wl_server::{
    connection::{Client, Objects},
    error,
    objects::{wayland_object, DISPLAY_ID},
};

use crate::{
    objects::Buffer as BufferObj,
    shell::buffers::{BufferBase, HasBuffer},
    utils::geometry::{coords, Extent},
};

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone, Copy)]
struct MapRecord {
    start: *const libc::c_void,
    len:   usize,
}

impl MapRecord {
    fn contains(&self, ptr: *const libc::c_void) -> bool {
        ptr >= self.start && ptr < unsafe { self.start.add(self.len) }
    }
}

// Safety: `start` is never dereferenced.
unsafe impl Send for MapRecord {}
// Safety: `start` is never dereferenced.
unsafe impl Sync for MapRecord {}

lazy_static::lazy_static! {
    /// List of all mapped regions. Used by the SIGBUS handler to decide whether a SIGBUS is
    /// generated because a client shrink its shm pool.
    ///
    /// # Regarding `Mutex`
    ///
    /// Using `std::sync::Mutex` can be undefined behavior in signal handlers, so we use a spin
    /// lock here instead.
    static ref MAP_RECORDS: Mutex<VecList<MapRecord>> = Mutex::new(VecList::new());
}

thread_local! {
    static SIGBUS_COUNT: Cell<usize> = Cell::new(0);
}

/// The number of times a recoverable SIGBUS has occurred for the current
/// thread. Can be used to detect if a client shrunk its shm pool.
pub fn sigbus_count() -> usize {
    SIGBUS_COUNT.with(|c| c.get())
}

unsafe fn map_zeroed(addr: *const libc::c_void, len: usize) -> Result<(), libc::c_int> {
    unsafe {
        libc::munmap(addr as *mut libc::c_void, len);
    }
    let ret = unsafe {
        libc::mmap(
            addr as *mut libc::c_void,
            len,
            libc::PROT_READ,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_FIXED,
            -1,
            0,
        )
    };
    if ret == libc::MAP_FAILED {
        Err(unsafe { *libc::__errno_location() })
    } else {
        Ok(())
    }
}

/// Handle a SIGBUS signal. Tries to recover from SIGBUS caused by a client
/// shrinking its shm pool. You MUST call this function is your SIGBUS handler
/// if you want to map shm pools.
///
/// Returns `true` if the signal was handled, `false` otherwise. Usually you
/// should reraise the signal if this function returns `false`.
///
/// # Safety
///
/// Must be called from a SIGBUS handler, with `info` provided to the signal
/// handler.
pub unsafe fn handle_sigbus(info: &libc::siginfo_t) -> bool {
    let faulty_ptr = unsafe { info.si_addr() } as *const libc::c_void;
    // # Regarding deadlocks
    //
    // This function will deadlock if a SIGBUS occurs while the current thread is
    // holding a lock on `MAP_RECORDS`.
    // The only thing we do while holding this lock is accessing the `VecList`
    // inside, and it should be completely safe and never trigger a SIGBUS.
    let records = MAP_RECORDS.lock();
    if let Some(record) = records.iter().find(|r| r.contains(faulty_ptr)) {
        SIGBUS_COUNT.with(|c| c.set(c.get() + 1));
        unsafe { map_zeroed(record.start, record.len) }.is_ok()
    } else {
        false
    }
}

#[derive(Default, Debug)]
pub struct Shm;

pub enum ShmError {
    Mapping(u32, i32),
}

impl std::fmt::Debug for ShmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ShmError::Mapping(_, err) => write!(f, "Mapping error: {err}"),
        }
    }
}

impl std::fmt::Display for ShmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for ShmError {}

impl wl_protocol::ProtocolError for ShmError {
    fn fatal(&self) -> bool {
        match self {
            ShmError::Mapping(..) => true,
        }
    }

    fn wayland_error(&self) -> Option<(u32, u32)> {
        match self {
            ShmError::Mapping(object_id, _) =>
                Some((*object_id, wl_shm::enums::Error::InvalidFd as u32)),
        }
    }
}

// TODO: Add a trait for ShmPool and make Shm generic over the pool type.

#[wayland_object]
impl<Ctx> wl_shm::RequestDispatch<Ctx> for Shm
where
    Ctx: Client,
    Ctx::Object: From<ShmPool>,
{
    type Error = error::Error;

    type CreatePoolFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;

    fn create_pool<'a>(
        ctx: &'a mut Ctx,
        object_id: u32,
        id: wl_types::NewId,
        mut fd: wl_types::Fd,
        size: i32,
    ) -> Self::CreatePoolFut<'a> {
        tracing::debug!("creating shm_pool with size {}", size);
        async move {
            use wl_server::connection::WriteMessage;
            if size <= 0 {
                ctx.connection()
                    .send(DISPLAY_ID, wl_display::events::Error {
                        code:      wl_shm::enums::Error::InvalidStride as u32,
                        object_id: object_id.into(),
                        message:   wl_types::str!("invalid size"),
                    })
                    .await?;
                return Ok(())
            }
            let fd = unsafe {
                fd.assume_owned();
                fd.take().unwrap_unchecked()
            };
            // Safety: mapping the file descriptor is harmless until we try to access it.
            let addr = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    size as usize,
                    libc::PROT_READ,
                    libc::MAP_SHARED,
                    fd.as_raw_fd(),
                    0,
                )
            };
            if addr == libc::MAP_FAILED {
                return Err(error::Error::custom(ShmError::Mapping(object_id, unsafe {
                    *libc::__errno_location()
                })))
            }
            let pool = ShmPool {
                inner: Rc::new(RefCell::new(ShmPoolInner {
                    fd,
                    addr,
                    len: size as usize,
                    map: MAP_RECORDS.lock().push_back(MapRecord {
                        start: addr,
                        len:   size as usize,
                    }),
                })),
            };
            if ctx.objects().insert(id.0, pool).is_err() {
                ctx.connection()
                    .send(DISPLAY_ID, wl_display::events::Error {
                        code:      wl_display::enums::Error::InvalidObject as u32,
                        object_id: object_id.into(),
                        message:   wl_types::str!("id already in use"),
                    })
                    .await?;
            }
            Ok(())
        }
    }
}

impl Drop for ShmPoolInner {
    fn drop(&mut self) {
        self.unmap()
    }
}

impl ShmPoolInner {
    // Safety:  caller must ensure all the requirements states on [`ShmPool::map`]
    // are met.
    unsafe fn as_ref(&self) -> &[u8] {
        tracing::debug!("mapping shm_pool {:p}, size {}", self, self.len);
        assert!(!self.addr.is_null());
        unsafe { std::slice::from_raw_parts(self.addr as *const u8, self.len) }
    }

    fn unmap(&mut self) {
        // we might already be unmapped. e.g. if `resize` failed.
        if let Some(record) = MAP_RECORDS.lock().remove(self.map) {
            // Safety: `unmap` takes an exclusive reference, meaning no one can be holding
            // the slice returned by `as_ref`.
            unsafe {
                libc::munmap(record.start as *mut libc::c_void, record.len);
            }
            self.addr = std::ptr::null();
            self.len = 0;
        }
    }
}

#[derive(Debug)]
pub struct ShmPoolInner {
    fd:   OwnedFd,
    addr: *const libc::c_void,
    len:  usize,
    map:  Index<MapRecord>,
}

#[derive(Debug)]
pub struct ShmPool {
    inner: Rc<RefCell<ShmPoolInner>>,
}

impl ShmPool {
    /// Map the pool into memory.
    ///
    /// This can be called repeatedly to retrieve the slice whenever you need
    /// it. The map operation is only performed once.
    ///
    /// # Safety
    ///
    /// The file descriptor MUST be suitable for mapping.
    ///
    /// You MUST setup a SIGBUS handler that calls `handle_sigbus`. Otherwise if
    /// the client shrunk the pool after you have mapped it, you will get a
    /// SIGBUS when accessing the removed section of memory. `handle_sigbus`
    /// will automatcally map in zero pages in that case.
    pub unsafe fn map(&self) -> Ref<[u8]> {
        Ref::map(self.inner.borrow(), |inner: &ShmPoolInner| inner.as_ref())
    }
}

#[wayland_object]
impl<Ctx> wl_shm_pool::RequestDispatch<Ctx> for ShmPool
where
    Ctx: Client,
    Ctx::ServerContext: HasBuffer,
    <Ctx::ServerContext as HasBuffer>::Buffer: From<Buffer>,
    Ctx::Object: From<BufferObj<<Ctx::ServerContext as HasBuffer>::Buffer>>,
{
    type Error = error::Error;

    type CreateBufferFut<'a> = impl Future<Output = Result<(), error::Error>> + 'a where Ctx: 'a;
    type DestroyFut<'a> = impl Future<Output = Result<(), error::Error>> + 'a where Ctx: 'a;
    type ResizeFut<'a> = impl Future<Output = Result<(), error::Error>> + 'a where Ctx: 'a;

    fn create_buffer<'a>(
        ctx: &'a mut Ctx,
        object_id: u32,
        id: wl_types::NewId,
        offset: i32,
        width: i32,
        height: i32,
        stride: i32,
        format: wl_protocol::wayland::wl_shm::v1::enums::Format,
    ) -> Self::CreateBufferFut<'a> {
        async move {
            use wl_server::connection::Objects;
            let pool = {
                use wl_server::objects::Object;
                let this = ctx.objects().get(object_id).unwrap();
                let this: &Self = this.cast().unwrap();
                this.inner.clone()
            };

            let inserted = ctx.objects().try_insert_with(id.0, || {
                let buffer: BufferObj<<Ctx::ServerContext as HasBuffer>::Buffer> = BufferObj {
                    buffer: Rc::new(
                        Buffer {
                            base: BufferBase::new(id.0),
                            pool,
                            offset,
                            width,
                            height,
                            stride,
                            format,
                        }
                        .into(),
                    ),
                };
                buffer.into()
            });
            if !inserted {
                Err(wl_server::error::Error::IdExists(id.0))
            } else {
                Ok(())
            }
        }
    }

    fn destroy<'a>(ctx: &'a mut Ctx, object_id: u32) -> Self::DestroyFut<'a> {
        async move {
            use wl_server::connection::WriteMessage;
            ctx.objects().remove(object_id).unwrap();
            ctx.connection()
                .send(DISPLAY_ID, wl_display::events::DeleteId { id: object_id })
                .await?;
            Ok(())
        }
    }

    fn resize<'a>(ctx: &'a mut Ctx, object_id: u32, size: i32) -> Self::ResizeFut<'a> {
        async move {
            let len = size as usize;
            let pool = {
                use wl_server::objects::Object;
                let this = ctx.objects().get(object_id).unwrap();
                let this: &Self = this.cast().unwrap();
                this.inner.clone()
            };
            let mut inner = pool.borrow_mut();
            tracing::debug!("resize shm_pool {:p} to {}", &*inner, size);
            if len > inner.len {
                let fd = inner.fd.as_raw_fd();
                inner.unmap();

                // Safety: mapping the file descriptor is harmless until we try to access it.
                let addr = unsafe {
                    libc::mmap(
                        std::ptr::null_mut(),
                        len,
                        libc::PROT_READ,
                        libc::MAP_SHARED,
                        fd,
                        0,
                    )
                };
                if addr == libc::MAP_FAILED {
                    return Err(error::Error::UnknownFatalError("mmap failed"))
                }

                inner.addr = addr;
                inner.len = len;
                // update th map record
                inner.map = MAP_RECORDS.lock().push_back(MapRecord { start: addr, len });
            }
            Ok(())
        }
    }
}

#[derive(Debug)]
pub struct Buffer {
    base:   BufferBase,
    pool:   Rc<RefCell<ShmPoolInner>>,
    offset: i32,
    width:  i32,
    height: i32,
    stride: i32,
    format: wl_shm::enums::Format,
}

impl crate::shell::buffers::Buffer for Buffer {
    fn damage(&self) {
        self.base.damage();
    }

    fn clear_damage(&self) {
        self.base.clear_damage();
    }

    fn get_damage(&self) -> bool {
        self.base.get_damage()
    }

    fn dimension(&self) -> Extent<u32, coords::Buffer> {
        Extent::new(self.width as u32, self.height as u32)
    }

    fn object_id(&self) -> u32 {
        self.base.object_id()
    }
}

impl Buffer {
    pub fn pool(&self) -> ShmPool {
        ShmPool {
            inner: self.pool.clone(),
        }
    }

    pub fn offset(&self) -> i32 {
        self.offset
    }

    pub fn width(&self) -> i32 {
        self.width
    }

    pub fn height(&self) -> i32 {
        self.height
    }

    pub fn stride(&self) -> i32 {
        self.stride
    }

    pub fn format(&self) -> wl_shm::enums::Format {
        self.format
    }
}
