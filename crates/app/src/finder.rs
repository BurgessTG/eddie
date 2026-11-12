//! Files macOS hands the app: a project double-clicked in Finder, media
//! dropped on the Dock icon, `open -a Eddie cut.eddie`.
//!
//! They arrive as an open-documents Apple event, not as arguments, whether
//! Eddie is starting or already running. winit's application delegate does
//! not answer it and Rune listens only for links, so AppKit would say the
//! document could not be opened. Eddie takes the event itself.
//!
//! AppKit installs its own handlers while it launches and a handler set
//! earlier is replaced, so ours goes in when the application is about to
//! finish launching. The event for the file that started the app comes
//! after that and before the window exists.

use std::path::PathBuf;
use std::sync::OnceLock;

use objc2::rc::autoreleasepool;
use objc2::runtime::{AnyObject, ClassBuilder, NSObject, Sel};
use objc2::{ClassType, class, msg_send, sel};
use objc2_foundation::NSString;

/// `kCoreEventClass`.
const CORE_EVENT: u32 = u32::from_be_bytes(*b"aevt");
/// `kAEOpenDocuments`.
const OPEN_DOCUMENTS: u32 = u32::from_be_bytes(*b"odoc");
/// `keyDirectObject`: the event's files.
const DIRECT_OBJECT: u32 = u32::from_be_bytes(*b"----");

type Sink = Box<dyn Fn(Vec<PathBuf>) + Send + Sync>;

static SINK: OnceLock<Sink> = OnceLock::new();

/// Send every file the system opens with Eddie to `sink`, on the main
/// thread. Call once, on the main thread, before the event loop starts.
pub fn listen(sink: impl Fn(Vec<PathBuf>) + Send + Sync + 'static) {
    if SINK.set(Box::new(sink)).is_err() {
        return;
    }
    let Some(mut builder) = ClassBuilder::new("EddieOpenedFiles", NSObject::class()) else {
        return;
    };
    // SAFETY: each function takes what its selector passes: one notification,
    // or an event and its reply, and returns nothing.
    unsafe {
        builder.add_method(
            sel!(willFinishLaunching:),
            will_finish_launching as extern "C" fn(_, _, _),
        );
        builder.add_method(
            sel!(openDocuments:withReplyEvent:),
            open_documents as extern "C" fn(_, _, _, _),
        );
    }
    let class = builder.register();
    let name = NSString::from_str("NSApplicationWillFinishLaunchingNotification");
    // SAFETY: Foundation calls on the main thread. The observer is never
    // released: neither the notification centre nor the event manager
    // retains it, and it is needed for as long as the app runs.
    unsafe {
        let observer: *mut AnyObject = msg_send![class, new];
        let centre: *mut AnyObject = msg_send![class!(NSNotificationCenter), defaultCenter];
        let nothing: *const AnyObject = std::ptr::null();
        let _: () = msg_send![centre, addObserver: observer, selector: sel!(willFinishLaunching:),
            name: &*name, object: nothing];
    }
}

extern "C" fn will_finish_launching(this: &AnyObject, _: Sel, _notification: *mut AnyObject) {
    // SAFETY: Foundation calls on the main thread; `this` lives for the
    // process (see `listen`).
    unsafe {
        let manager: *mut AnyObject =
            msg_send![class!(NSAppleEventManager), sharedAppleEventManager];
        let _: () = msg_send![manager, setEventHandler: this,
            andSelector: sel!(openDocuments:withReplyEvent:),
            forEventClass: CORE_EVENT, andEventID: OPEN_DOCUMENTS];
    }
}

extern "C" fn open_documents(_: &AnyObject, _: Sel, event: *mut AnyObject, _reply: *mut AnyObject) {
    // SAFETY: AppKit passes the event's NSAppleEventDescriptor, or nil.
    let paths = unsafe { event.as_ref() }.map(paths_of).unwrap_or_default();
    if let (Some(sink), false) = (SINK.get(), paths.is_empty()) {
        sink(paths);
    }
}

/// The files an open-documents event names. Finder sends a list; a script's
/// `open` may send one file on its own.
fn paths_of(event: &AnyObject) -> Vec<PathBuf> {
    // SAFETY: messages to NSAppleEventDescriptor, NSURL and NSString, each
    // returning an autoreleased object or nil, released with the pool.
    autoreleasepool(|_| unsafe {
        let files: *mut AnyObject = msg_send![event, paramDescriptorForKeyword: DIRECT_OBJECT];
        let Some(files) = files.as_ref() else {
            return Vec::new();
        };
        let count: isize = msg_send![files, numberOfItems];
        if count == 0 {
            return path_of(files).into_iter().collect();
        }
        // Apple event lists count from 1.
        (1..=count)
            .filter_map(|index| {
                let file: *mut AnyObject = msg_send![files, descriptorAtIndex: index];
                path_of(file.as_ref()?)
            })
            .collect()
    })
}

/// # Safety
/// `file` is an NSAppleEventDescriptor, inside an autorelease pool.
unsafe fn path_of(file: &AnyObject) -> Option<PathBuf> {
    unsafe {
        let url: *mut AnyObject = msg_send![file, fileURLValue];
        let path: *mut NSString = msg_send![url.as_ref()?, path];
        Some(PathBuf::from(path.as_ref()?.to_string()))
    }
}
