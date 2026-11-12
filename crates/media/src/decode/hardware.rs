//! Hardware decode for playback lanes (VAAPI on Linux, VideoToolbox on
//! macOS), and the switch back to software when a driver fails.

use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use ffmpeg_next as ffmpeg;

use super::{ensure_init, err};
use crate::MediaResult;

/// Settings → Hardware decode. Only preview playback lanes use it.
pub(super) static HARDWARE_DECODE: AtomicBool = AtomicBool::new(false);

/// Set by the first hardware failure; software decodes for the session.
static HARDWARE_BROKEN: AtomicBool = AtomicBool::new(false);

/// The failure not yet reported to the app.
static HARDWARE_FAILURE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Test hook: playback decoders act as hardware ones and fail mid-stream.
pub(super) static INJECT_FAULT: AtomicBool = AtomicBool::new(false);

/// Stop using hardware decode for this session after `error`.
pub(crate) fn hardware_failed(error: &str) {
    if !HARDWARE_BROKEN.swap(true, Ordering::Relaxed) {
        *HARDWARE_FAILURE.lock().unwrap() = Some(error.to_owned());
    }
}

pub(crate) fn hardware_broken() -> bool {
    HARDWARE_BROKEN.load(Ordering::Relaxed)
}

/// The hardware decode failure that switched preview to software, once.
pub fn take_hardware_failure() -> Option<String> {
    HARDWARE_FAILURE.lock().unwrap().take()
}

/// Make playback decoders behave as hardware decoders that fail after a few
/// pictures, so the software fallback can be tested without a GPU.
#[doc(hidden)]
pub fn inject_hardware_fault(on: bool) {
    INJECT_FAULT.store(on, Ordering::Relaxed);
    if !on {
        HARDWARE_BROKEN.store(false, Ordering::Relaxed);
    }
}

/// Allow or forbid GPU decode for decoders opened from now on.
pub fn set_hardware_decode(on: bool) {
    HARDWARE_DECODE.store(on, Ordering::Relaxed);
}

/// Files software decodes cheaply stay in software: on a Ryzen 3700X with
/// NVDEC through VAAPI, hardware cut 1440p H.264 CPU from 530-630% to 99%
/// but slowed its seeks from 160-195 to 285 ms, while 4K HEVC Main10 went
/// from 1116-1193% to 103% CPU with seeks 3.7x faster.
fn wants_hardware(codec: ffmpeg::codec::Id, width: i32, height: i32) -> bool {
    use ffmpeg::codec::Id;
    matches!(codec, Id::HEVC | Id::AV1 | Id::VP9)
        || i64::from(width) * i64::from(height) > 2560 * 1440
}

type SyncSurface = unsafe extern "C" fn(*mut std::ffi::c_void, u32) -> i32;

/// libva's `vaSyncSurface`, when the FFmpeg libraries brought libva in.
/// Looked up at run time so builds need no VA headers or libraries.
fn va_sync_surface() -> Option<SyncSurface> {
    #[cfg(target_os = "linux")]
    {
        extern "C" {
            fn dlsym(
                handle: *mut std::ffi::c_void,
                symbol: *const std::ffi::c_char,
            ) -> *mut std::ffi::c_void;
        }
        static SYNC: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
        // SAFETY: a null handle is RTLD_DEFAULT, the global symbol scope.
        let address = *SYNC.get_or_init(|| unsafe {
            dlsym(std::ptr::null_mut(), c"vaSyncSurface".as_ptr()) as usize
        });
        // SAFETY: the symbol is libva's
        // `VAStatus vaSyncSurface(VADisplay, VASurfaceID)`.
        (address != 0).then(|| unsafe { std::mem::transmute::<usize, SyncSurface>(address) })
    }
    #[cfg(not(target_os = "linux"))]
    None
}

/// One hardware device, shared by every decoder: creating a VAAPI one costs
/// 250-600 ms. Null when there is none.
fn device() -> *mut ffmpeg::ffi::AVBufferRef {
    struct Device(*mut ffmpeg::ffi::AVBufferRef);
    // SAFETY: the reference is created once, never freed, and only passed to
    // av_buffer_ref, which is thread-safe.
    unsafe impl Send for Device {}
    unsafe impl Sync for Device {}
    static DEVICE: std::sync::OnceLock<Device> = std::sync::OnceLock::new();
    DEVICE
        .get_or_init(|| {
            ensure_init();
            Device(open_device())
        })
        .0
}

/// VAAPI on the first DRM render node that opens.
#[cfg(target_os = "linux")]
fn open_device() -> *mut ffmpeg::ffi::AVBufferRef {
    if va_sync_surface().is_none() {
        return std::ptr::null_mut();
    }
    let mut nodes: Vec<_> = std::fs::read_dir("/dev/dri")
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("renderD"))
        })
        .collect();
    nodes.sort();
    for node in nodes {
        let Ok(node) = std::ffi::CString::new(node.as_os_str().as_encoded_bytes()) else {
            continue;
        };
        let mut device = std::ptr::null_mut();
        // SAFETY: on failure the pointer stays null.
        let result = unsafe {
            ffmpeg::ffi::av_hwdevice_ctx_create(
                &mut device,
                ffmpeg::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI,
                node.as_ptr(),
                std::ptr::null_mut(),
                0,
            )
        };
        if result >= 0 {
            return device;
        }
    }
    std::ptr::null_mut()
}

/// VideoToolbox, which every Mac has.
#[cfg(target_os = "macos")]
fn open_device() -> *mut ffmpeg::ffi::AVBufferRef {
    let mut device = std::ptr::null_mut();
    // SAFETY: on failure the pointer stays null.
    unsafe {
        ffmpeg::ffi::av_hwdevice_ctx_create(
            &mut device,
            ffmpeg::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_VIDEOTOOLBOX,
            std::ptr::null(),
            std::ptr::null_mut(),
            0,
        );
    }
    device
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn open_device() -> *mut ffmpeg::ffi::AVBufferRef {
    std::ptr::null_mut()
}

/// Open the shared device off the UI path, so the first play does not wait.
pub fn warm_hardware_decode() {
    if HARDWARE_DECODE.load(Ordering::Relaxed) {
        thread::spawn(|| {
            device();
        });
    }
}

/// Give a preview decoder the hardware device when hardware decode is on
/// and the file is expensive in software. libavcodec decodes on the GPU when
/// the profile is supported (NVDEC has no 4:2:2) and silently falls back to
/// software otherwise.
pub(super) fn attach(context: &mut ffmpeg::codec::context::Context) -> bool {
    if !HARDWARE_DECODE.load(Ordering::Relaxed) || hardware_broken() {
        return false;
    }
    // SAFETY: plain reads of the unopened context's parameters.
    let (codec, width, height) = unsafe {
        let raw = context.as_ptr();
        (
            ffmpeg::codec::Id::from((*raw).codec_id),
            (*raw).width,
            (*raw).height,
        )
    };
    if !wants_hardware(codec, width, height) {
        return false;
    }
    let device = device();
    if device.is_null() {
        return false;
    }
    // SAFETY: the codec context is not open yet; it takes its own reference.
    unsafe {
        let raw = context.as_mut_ptr();
        (*raw).hw_device_ctx = ffmpeg::ffi::av_buffer_ref(device);
        // Preview lanes hold the current picture and one lookahead; two
        // more cover the frame being decoded and the one being downloaded.
        (*raw).extra_hw_frames = 4;
    }
    true
}

/// Wait until a decoded VAAPI surface is complete, as it leaves the decoder.
/// nvidia-vaapi-driver (0.0.18) queues every finished picture on a 16-slot
/// ring with no overflow check; pictures decoded ahead and never synced (seek
/// preroll, frame threads) lap it, and a lost picture never completes, so a
/// later sync on it waits forever. Syncing here keeps the ring drained, and
/// a seek can no longer return a reused surface before it is rewritten.
pub(super) fn sync_surface(picture: &ffmpeg::frame::Video) -> MediaResult<()> {
    if picture.format() != ffmpeg::format::Pixel::VAAPI {
        return Ok(());
    }
    let sync = va_sync_surface().ok_or("libva is not loaded")?;
    // SAFETY: a VAAPI frame carries its surface ID in data[3] and a frames
    // context whose device context is an AVVAAPIDeviceContext.
    let status = unsafe {
        let frame = picture.as_ptr();
        let frames = (*(*frame).hw_frames_ctx).data as *const ffmpeg::ffi::AVHWFramesContext;
        let device = (*(*frames).device_ctx).hwctx as *const ffmpeg::ffi::AVVAAPIDeviceContext;
        sync(
            (*device).display as *mut _,
            (*frame).data[3] as usize as u32,
        )
    };
    if status != 0 {
        return Err(format!("sync surface: VA status {status}"));
    }
    Ok(())
}

/// Whether a decoded picture is still on the GPU and needs `download`.
pub(super) fn on_gpu(picture: &ffmpeg::frame::Video) -> bool {
    matches!(
        picture.format(),
        ffmpeg::format::Pixel::VAAPI | ffmpeg::format::Pixel::VIDEOTOOLBOX
    )
}

/// Copy a GPU surface into `into` (NV12 or P010) with its timing and
/// colour. `into` keeps its buffers between pictures of the same size: a
/// fresh 4K allocation per picture cost page faults on every frame.
pub(super) fn download(
    picture: &ffmpeg::frame::Video,
    into: &mut ffmpeg::frame::Video,
) -> MediaResult<()> {
    // SAFETY: both frames are owned and `into` is never shared, so its
    // buffers are writable; transfer allocates them when it has none.
    unsafe {
        let (src, dst) = (picture.as_ptr(), into.as_mut_ptr());
        if (*dst).buf[0].is_null() || ((*dst).width, (*dst).height) != ((*src).width, (*src).height)
        {
            ffmpeg::ffi::av_frame_unref(dst);
        }
        let result = ffmpeg::ffi::av_hwframe_transfer_data(dst, src, 0);
        if result < 0 {
            return Err(err("download surface", ffmpeg::Error::from(result)));
        }
        (*dst).pts = (*src).pts;
        (*dst).color_range = (*src).color_range;
        (*dst).colorspace = (*src).colorspace;
        (*dst).color_primaries = (*src).color_primaries;
        (*dst).color_trc = (*src).color_trc;
        (*dst).chroma_location = (*src).chroma_location;
    }
    Ok(())
}
