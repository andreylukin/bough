//! Invariant: the IOKit registration lives on ITS OWN THREAD with its own `CFRunLoop`, and the
//! sleep acknowledgement (`IOAllowPowerChange`) is sent IMMEDIATELY on
//! `kIOMessageSystemWillSleep` — before any bough work — because the system waits on it and a slow
//! acknowledgement is a visible stall for Andrey.
//!
//! Teardown stops the run loop and joins the thread; nothing is left registered.
//!
//! Everything here is hand-rolled FFI (§13 says so in as many words). The two rules that keep it
//! honest: no allocation crosses a thread boundary except through an `Arc`, and every `objc_msgSend`
//! is called through a POINTER CAST TO ITS EXACT SIGNATURE — the variadic declaration is wrong on
//! aarch64 and calling it that way corrupts arguments.

use std::ffi::{c_void, CString};
use std::sync::mpsc;
use std::sync::Arc;

use crate::Gate;

// ---- IOKit ---------------------------------------------------------------

type IoConnect = u32;
type IoObject = u32;
type IoNotificationPortRef = *mut c_void;
type CfRunLoopRef = *mut c_void;
type CfRunLoopSourceRef = *mut c_void;
type CfStringRef = *const c_void;

/// `iokit_common_msg(x) == 0xe0000000 | x`.
const K_IO_MESSAGE_CAN_SYSTEM_SLEEP: u32 = 0xe000_0270;
const K_IO_MESSAGE_SYSTEM_WILL_SLEEP: u32 = 0xe000_0280;
const K_IO_MESSAGE_SYSTEM_HAS_POWERED_ON: u32 = 0xe000_0300;

type IoServiceInterestCallback =
    extern "C" fn(refcon: *mut c_void, service: IoObject, message_type: u32, argument: *mut c_void);

#[link(name = "IOKit", kind = "framework")]
extern "C" {
    fn IORegisterForSystemPower(
        refcon: *mut c_void,
        port: *mut IoNotificationPortRef,
        callback: IoServiceInterestCallback,
        notifier: *mut IoObject,
    ) -> IoConnect;
    fn IODeregisterForSystemPower(notifier: *mut IoObject) -> i32;
    fn IONotificationPortGetRunLoopSource(port: IoNotificationPortRef) -> CfRunLoopSourceRef;
    fn IONotificationPortDestroy(port: IoNotificationPortRef);
    fn IOAllowPowerChange(kernel_port: IoConnect, notification_id: isize) -> i32;
    fn IOServiceClose(connect: IoConnect) -> i32;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFRunLoopDefaultMode: CfStringRef;
    fn CFRunLoopGetCurrent() -> CfRunLoopRef;
    fn CFRunLoopAddSource(rl: CfRunLoopRef, source: CfRunLoopSourceRef, mode: CfStringRef);
    fn CFRunLoopRemoveSource(rl: CfRunLoopRef, source: CfRunLoopSourceRef, mode: CfStringRef);
    fn CFRunLoopRun();
    fn CFRunLoopStop(rl: CfRunLoopRef);
}

/// What the callback is handed. Reached from the run-loop thread ONLY, through a raw pointer this
/// struct's owner keeps alive until after the thread is joined.
struct IokitShared {
    gate: Arc<Gate>,
    /// The connection the acknowledgement goes back on.
    root_port: parking_lot::Mutex<IoConnect>,
}

extern "C" fn power_callback(
    refcon: *mut c_void,
    _service: IoObject,
    message_type: u32,
    argument: *mut c_void,
) {
    // SAFETY: `refcon` is the `Arc<IokitShared>` this source leaked at registration; it outlives
    // every callback because the source drops it only after the run loop stopped and the thread
    // was joined. The Arc is BORROWED, never consumed.
    let shared: &IokitShared = unsafe { &*(refcon as *const IokitShared) };
    let port = *shared.root_port.lock();
    match message_type {
        // The system is WAITING on this acknowledgement. Nothing bough does may come first.
        K_IO_MESSAGE_CAN_SYSTEM_SLEEP | K_IO_MESSAGE_SYSTEM_WILL_SLEEP => {
            unsafe { IOAllowPowerChange(port, argument as isize) };
            if message_type == K_IO_MESSAGE_SYSTEM_WILL_SLEEP {
                shared.gate.will_sleep(chrono::Utc::now());
            }
        }
        K_IO_MESSAGE_SYSTEM_HAS_POWERED_ON => {
            shared.gate.did_wake(chrono::Utc::now());
        }
        _ => {}
    }
}

/// The IOKit-backed hook. Dropping it stops the run loop and joins the thread.
pub struct IokitSource {
    run_loop: parking_lot::Mutex<Option<usize>>,
    thread: parking_lot::Mutex<Option<std::thread::JoinHandle<()>>>,
    /// The leaked `Arc<IokitShared>`, reclaimed after the join.
    refcon: parking_lot::Mutex<Option<*const IokitShared>>,
}

// SAFETY: the only raw pointer is the leaked `Arc<IokitShared>`, which is `Send + Sync` in its own
// right; it is dereferenced on the run-loop thread and reclaimed on the owner's, never both.
unsafe impl Send for IokitSource {}
unsafe impl Sync for IokitSource {}

impl IokitSource {
    /// Register with IOKit and start the run loop on a dedicated thread. `Err` when
    /// `IORegisterForSystemPower` returns a null port — the caller then falls back to NSWorkspace.
    pub fn start(gate: Arc<Gate>) -> Result<Arc<IokitSource>, String> {
        let shared = Arc::new(IokitShared {
            gate,
            root_port: parking_lot::Mutex::new(0),
        });
        let refcon = Arc::into_raw(Arc::clone(&shared));

        let source = Arc::new(IokitSource {
            run_loop: parking_lot::Mutex::new(None),
            thread: parking_lot::Mutex::new(None),
            refcon: parking_lot::Mutex::new(Some(refcon)),
        });

        let (tx, rx) = mpsc::channel::<Result<usize, String>>();
        let thread_shared = Arc::clone(&shared);
        let refcon_addr = refcon as usize;
        let handle = std::thread::Builder::new()
            .name("bough-sleep-listener".into())
            .spawn(move || {
                let refcon = refcon_addr as *mut c_void;
                let mut port: IoNotificationPortRef = std::ptr::null_mut();
                let mut notifier: IoObject = 0;
                // SAFETY: `port`/`notifier` are out-parameters; `refcon` outlives the run loop.
                let root_port = unsafe {
                    IORegisterForSystemPower(refcon, &mut port, power_callback, &mut notifier)
                };
                if root_port == 0 || port.is_null() {
                    let _ = tx.send(Err("IORegisterForSystemPower returned no port".to_string()));
                    return;
                }
                *thread_shared.root_port.lock() = root_port;
                // SAFETY: the port is non-null and this thread owns the run loop it is added to.
                let (rl, rl_source) = unsafe {
                    let rl_source = IONotificationPortGetRunLoopSource(port);
                    let rl = CFRunLoopGetCurrent();
                    CFRunLoopAddSource(rl, rl_source, kCFRunLoopDefaultMode);
                    (rl, rl_source)
                };
                if tx.send(Ok(rl as usize)).is_err() {
                    // Nobody is holding the source; unwind rather than run a loop forever.
                    unsafe {
                        CFRunLoopRemoveSource(rl, rl_source, kCFRunLoopDefaultMode);
                        IODeregisterForSystemPower(&mut notifier);
                        IOServiceClose(root_port);
                        IONotificationPortDestroy(port);
                    }
                    return;
                }
                // SAFETY: returns when another thread calls `CFRunLoopStop` on `rl`.
                unsafe { CFRunLoopRun() };
                // SAFETY: teardown, in the reverse order of registration, on the thread that
                // registered. Nothing is left registered.
                unsafe {
                    CFRunLoopRemoveSource(rl, rl_source, kCFRunLoopDefaultMode);
                    IODeregisterForSystemPower(&mut notifier);
                    IOServiceClose(root_port);
                    IONotificationPortDestroy(port);
                }
            })
            .map_err(|e| format!("could not start the sleep-listener thread: {e}"))?;

        match rx.recv() {
            Ok(Ok(rl)) => {
                *source.run_loop.lock() = Some(rl);
                *source.thread.lock() = Some(handle);
                Ok(source)
            }
            Ok(Err(e)) => {
                let _ = handle.join();
                // SAFETY: the thread is gone, so nothing can dereference the refcon again.
                unsafe { drop(Arc::from_raw(source.refcon.lock().take().unwrap())) };
                Err(e)
            }
            Err(_) => {
                let _ = handle.join();
                unsafe { drop(Arc::from_raw(source.refcon.lock().take().unwrap())) };
                Err("the sleep-listener thread died before registering".to_string())
            }
        }
    }

    /// Stop the run loop and join the thread.
    pub fn stop(&self) {
        if let Some(rl) = self.run_loop.lock().take() {
            // SAFETY: `rl` is the run loop the thread published; stopping a run loop from another
            // thread is the documented way to end `CFRunLoopRun`.
            unsafe { CFRunLoopStop(rl as CfRunLoopRef) };
        }
        if let Some(t) = self.thread.lock().take() {
            let _ = t.join();
        }
        if let Some(refcon) = self.refcon.lock().take() {
            // SAFETY: the thread is joined, so no callback can be running.
            unsafe { drop(Arc::from_raw(refcon)) };
        }
    }
}

impl Drop for IokitSource {
    fn drop(&mut self) {
        self.stop();
    }
}

// ---- NSWorkspace (the fallback) ------------------------------------------

type Id = *mut c_void;
type Sel = *const c_void;

#[link(name = "objc")]
extern "C" {
    fn objc_getClass(name: *const i8) -> Id;
    fn sel_registerName(name: *const i8) -> Sel;
    fn objc_allocateClassPair(superclass: Id, name: *const i8, extra: usize) -> Id;
    fn objc_registerClassPair(cls: Id);
    fn class_addMethod(cls: Id, sel: Sel, imp: *const c_void, types: *const i8) -> bool;
    fn objc_msgSend();
}

// AppKit is linked so `NSWorkspace` is a class the runtime can find.
#[link(name = "AppKit", kind = "framework")]
extern "C" {}

fn sel(name: &str) -> Sel {
    let c = CString::new(name).expect("a selector name has no interior NUL");
    // SAFETY: `c` is a valid NUL-terminated string for the length of the call.
    unsafe { sel_registerName(c.as_ptr()) }
}

fn class(name: &str) -> Id {
    let c = CString::new(name).expect("a class name has no interior NUL");
    // SAFETY: as above.
    unsafe { objc_getClass(c.as_ptr()) }
}

/// `objc_msgSend` with no arguments. Cast to its exact signature: the variadic declaration is
/// wrong on aarch64.
fn send0(target: Id, s: Sel) -> Id {
    // SAFETY: the cast matches the call being made, which is what the ABI requires.
    let f: extern "C" fn(Id, Sel) -> Id = unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    f(target, s)
}

fn send1(target: Id, s: Sel, a: Id) -> Id {
    let f: extern "C" fn(Id, Sel, Id) -> Id =
        unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    f(target, s, a)
}

fn send3(target: Id, s: Sel, a: Id, b: Id, c: Id) -> Id {
    let f: extern "C" fn(Id, Sel, Id, Id, Id) -> Id =
        unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    f(target, s, a, b, c)
}

fn send4(target: Id, s: Sel, a: Id, b: Sel, c: Id, d: Id) -> Id {
    let f: extern "C" fn(Id, Sel, Id, Sel, Id, Id) -> Id =
        unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    f(target, s, a, b, c, d)
}

fn nsstring(text: &str) -> Id {
    let c = CString::new(text).expect("no interior NUL");
    let cls = class("NSString");
    let f: extern "C" fn(Id, Sel, *const i8) -> Id =
        unsafe { std::mem::transmute(objc_msgSend as *const ()) };
    f(cls, sel("stringWithUTF8String:"), c.as_ptr())
}

/// Which observer instance belongs to which gate. A global because an Objective-C method receives
/// only `self`, and stashing a Rust pointer in an ivar buys nothing this map does not.
static OBSERVERS: parking_lot::Mutex<Vec<(usize, Arc<Gate>)>> = parking_lot::Mutex::new(Vec::new());

fn gate_of(this: Id) -> Option<Arc<Gate>> {
    OBSERVERS
        .lock()
        .iter()
        .find(|(p, _)| *p == this as usize)
        .map(|(_, g)| Arc::clone(g))
}

extern "C" fn on_sleep(this: Id, _cmd: Sel, _note: Id) {
    if let Some(g) = gate_of(this) {
        g.will_sleep(chrono::Utc::now());
    }
}

extern "C" fn on_wake(this: Id, _cmd: Sel, _note: Id) {
    if let Some(g) = gate_of(this) {
        g.did_wake(chrono::Utc::now());
    }
}

/// The observer class, registered ONCE per process: `objc_allocateClassPair` refuses a name that
/// already exists, and a row that reloads must not take that as a failure.
static OBSERVER_CLASS: std::sync::OnceLock<usize> = std::sync::OnceLock::new();

fn observer_class() -> Result<Id, String> {
    let cached = OBSERVER_CLASS.get_or_init(|| {
        let name = CString::new("BoughPowerObserver").expect("no NUL");
        // SAFETY: NSObject exists; the name is fresh the first time through this `OnceLock`.
        let cls = unsafe { objc_allocateClassPair(class("NSObject"), name.as_ptr(), 0) };
        if cls.is_null() {
            return 0;
        }
        let types = CString::new("v@:@").expect("no NUL");
        unsafe {
            class_addMethod(
                cls,
                sel("boughOnSleep:"),
                on_sleep as *const c_void,
                types.as_ptr(),
            );
            class_addMethod(
                cls,
                sel("boughOnWake:"),
                on_wake as *const c_void,
                types.as_ptr(),
            );
            objc_registerClassPair(cls);
        }
        cls as usize
    });
    if *cached == 0 {
        return Err("could not register the observer class".to_string());
    }
    Ok(*cached as Id)
}

/// The NSWorkspace FALLBACK. Used only when IOKit gives no port; it misses dark wakes.
pub struct NsWorkspaceSource {
    observer: usize,
}

// SAFETY: `observer` is an Objective-C object pointer; every use goes through `objc_msgSend`,
// which is thread-safe for the two calls this type makes.
unsafe impl Send for NsWorkspaceSource {}
unsafe impl Sync for NsWorkspaceSource {}

impl NsWorkspaceSource {
    /// Subscribe to `NSWorkspaceWillSleepNotification` / `DidWakeNotification`.
    pub fn start(gate: Arc<Gate>) -> Result<Arc<NsWorkspaceSource>, String> {
        let cls = observer_class()?;
        let observer = send0(send0(cls, sel("alloc")), sel("init"));
        if observer.is_null() {
            return Err("could not allocate the observer".to_string());
        }
        OBSERVERS.lock().push((observer as usize, gate));

        let workspace = class("NSWorkspace");
        if workspace.is_null() {
            return Err("NSWorkspace is not available in this process".to_string());
        }
        let center = send0(
            send0(workspace, sel("sharedWorkspace")),
            sel("notificationCenter"),
        );
        if center.is_null() {
            return Err("NSWorkspace has no notification center here".to_string());
        }
        let add = sel("addObserver:selector:name:object:");
        send4(
            center,
            add,
            observer,
            sel("boughOnSleep:"),
            nsstring("NSWorkspaceWillSleepNotification"),
            std::ptr::null_mut(),
        );
        send4(
            center,
            add,
            observer,
            sel("boughOnWake:"),
            nsstring("NSWorkspaceDidWakeNotification"),
            std::ptr::null_mut(),
        );
        Ok(Arc::new(NsWorkspaceSource {
            observer: observer as usize,
        }))
    }

    /// The notification center this source observes. Exposed so the macOS test can POST a
    /// notification into it rather than asking someone to close a lid.
    pub fn center() -> Id {
        send0(
            send0(class("NSWorkspace"), sel("sharedWorkspace")),
            sel("notificationCenter"),
        )
    }

    /// Post `name` into the workspace's notification center. Test-only: it is how the observer is
    /// exercised without a real sleep.
    pub fn post(name: &str) {
        let center = Self::center();
        send3(
            center,
            sel("postNotificationName:object:userInfo:"),
            nsstring(name),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        );
    }
}

impl Drop for NsWorkspaceSource {
    fn drop(&mut self) {
        let center = Self::center();
        if !center.is_null() {
            send1(center, sel("removeObserver:"), self.observer as Id);
        }
        OBSERVERS.lock().retain(|(p, _)| *p != self.observer);
    }
}
