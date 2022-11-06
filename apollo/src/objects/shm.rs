use std::{
    cell::{Cell, Ref, RefCell},
    future::Future,
    os::{fd::AsRawFd, unix::io::OwnedFd},
    rc::Rc,
    sync::atomic::AtomicUsize,
};

use dlv_list::{Index, VecList};
use spin::mutex::Mutex;
use wl_common::interface_message_dispatch;
use wl_protocol::wayland::{
    wl_buffer::v1 as wl_buffer, wl_display::v1 as wl_display, wl_shm::v1 as wl_shm,
    wl_shm_pool::v1 as wl_shm_pool,
};
use wl_server::{
    connection::{Connection, Objects},
    error,
    objects::{InterfaceMeta, DISPLAY_ID},
    provide_any::{self, Demand},
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
        if unsafe { map_zeroed(record.start, record.len) }.is_ok() {
            true
        } else {
            false
        }
    } else {
        false
    }
}

pub struct Shm;
impl Shm {
    pub fn new() -> Shm {
        Shm
    }
}
impl<Ctx> InterfaceMeta<Ctx> for Shm {
    fn interface(&self) -> &'static str {
        wl_shm::NAME
    }

    fn provide<'a>(&'a self, demand: &mut provide_any::Demand<'a>) {
        demand.provide_ref(self);
    }
}

pub enum ShmError {
    Mapping(u32, i32),
}

impl std::fmt::Debug for ShmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ShmError::Mapping(_, err) => write!(f, "Mapping error: {}", err),
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

#[interface_message_dispatch]
impl<Ctx> wl_shm::RequestDispatch<Ctx> for Shm
where
    Ctx: Connection,
{
    type Error = error::Error;

    type CreatePoolFut<'a> = impl Future<Output = Result<(), Self::Error>> + 'a where Ctx: 'a;

    fn create_pool<'a>(
        &'a self,
        ctx: &'a mut Ctx,
        object_id: u32,
        id: wl_types::NewId,
        mut fd: wl_types::Fd,
        size: i32,
    ) -> Self::CreatePoolFut<'a> {
        async move {
            if size <= 0 {
                ctx.send(DISPLAY_ID, wl_display::events::Error {
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
            if ctx.objects().borrow_mut().insert(id.0, pool).is_err() {
                ctx.send(DISPLAY_ID, wl_display::events::Error {
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
        assert!(self.addr != std::ptr::null());
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

pub struct ShmPoolInner {
    fd:   OwnedFd,
    addr: *const libc::c_void,
    len:  usize,
    map:  Index<MapRecord>,
}

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
    pub unsafe fn map<T>(&self) -> std::io::Result<Ref<[u8]>> {
        Ok(Ref::map(self.inner.borrow(), |inner: &ShmPoolInner| {
            inner.as_ref()
        }))
    }
}

impl<Ctx> InterfaceMeta<Ctx> for ShmPool {
    fn interface(&self) -> &'static str {
        wl_shm_pool::NAME
    }

    fn provide<'a>(&'a self, demand: &mut provide_any::Demand<'a>) {
        demand.provide_ref(self);
    }
}

#[interface_message_dispatch]
impl<Ctx> wl_shm_pool::RequestDispatch<Ctx> for ShmPool
where
    Ctx: Connection,
{
    type Error = error::Error;

    type CreateBufferFut<'a> = impl Future<Output = Result<(), error::Error>> + 'a where Ctx: 'a;
    type DestroyFut<'a> = impl Future<Output = Result<(), error::Error>> + 'a where Ctx: 'a;
    type ResizeFut<'a> = impl Future<Output = Result<(), error::Error>> + 'a where Ctx: 'a;

    fn resize<'a>(&'a self, _ctx: &'a mut Ctx, _object_id: u32, size: i32) -> Self::ResizeFut<'a> {
        async move {
            let len = size as usize;
            let mut inner = self.inner.borrow_mut();
            if len > inner.len {
                let fd = inner.fd.as_raw_fd();
                inner.len = len;
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
                // update th map record
                inner.map = MAP_RECORDS.lock().push_back(MapRecord { start: addr, len });
            }
            Ok(())
        }
    }

    fn create_buffer<'a>(
        &'a self,
        ctx: &'a mut Ctx,
        _object_id: u32,
        id: wl_types::NewId,
        offset: i32,
        width: i32,
        height: i32,
        stride: i32,
        format: wl_protocol::wayland::wl_shm::v1::enums::Format,
    ) -> Self::CreateBufferFut<'a> {
        async move {
            use wl_server::connection::{Entry, Objects};
            let mut objects = ctx.objects().borrow_mut();
            let entry = objects.entry(id.0);
            if entry.is_vacant() {
                entry.or_insert(Buffer {
                    base: crate::shell::buffers::BufferBase,
                    pool: self.inner.clone(),
                    offset,
                    width,
                    height,
                    stride,
                    format,
                });
                Ok(())
            } else {
                Err(wl_server::error::Error::IdExists(id.0))
            }
        }
    }

    fn destroy<'a>(&'a self, ctx: &'a mut Ctx, object_id: u32) -> Self::DestroyFut<'a> {
        async move { unimplemented!() }
    }
}

pub struct Buffer {
    base:   crate::shell::buffers::BufferBase,
    pool:   Rc<RefCell<ShmPoolInner>>,
    offset: i32,
    width:  i32,
    height: i32,
    stride: i32,
    format: wl_protocol::wayland::wl_shm::v1::enums::Format,
}

impl<Ctx> InterfaceMeta<Ctx> for Buffer {
    fn interface(&self) -> &'static str {
        wl_buffer::NAME
    }

    fn provide<'a>(&'a self, demand: &mut Demand<'a>) {
        demand
            .provide_ref(self)
            .provide_ref(&self.base);
    }
}
