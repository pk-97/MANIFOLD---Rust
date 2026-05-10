//! ScreenCaptureKit-based capture path.
//!
//! Replaces the deprecated CGDisplayStream FFI so we can:
//!   - Request a 16-bit float pixel format (`'RGhA'` = `kCVPixelFormatType_64RGBAHalf`)
//!     to preserve HDR highlights past 1.0 linear luminance.
//!   - Set `colorSpaceName = kCGColorSpaceExtendedLinearSRGB` so the data is
//!     delivered in linear extended-range sRGB (no sRGB encoding to undo,
//!     wide-gamut + super-white values pass through).
//!   - Set `captureDynamicRange = HDRLocalDisplay` so macOS doesn't tone-map
//!     the captured display down to SDR before handing it to us.
//!
//! When HDR is off, falls back to BGRA8 (matches the old CGDisplayStream
//! behaviour) so the existing slicer path still works.

use std::ffi::c_void;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use block2::RcBlock;
use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{AllocAnyThread, define_class, msg_send};
use objc2_core_media::CMSampleBuffer;
use objc2_foundation::{NSArray, NSError, NSString};
use objc2_screen_capture_kit::{
    SCCaptureDynamicRange, SCContentFilter, SCShareableContent, SCStream, SCStreamConfiguration,
    SCStreamOutput, SCStreamOutputType,
};

use crate::SharedState;

// ─── FFI: CMSampleBuffer / CVPixelBuffer extraction ──────────────────────────

#[link(name = "CoreMedia", kind = "framework")]
unsafe extern "C" {
    fn CMSampleBufferGetImageBuffer(sample: *const c_void) -> *const c_void;
}

#[link(name = "CoreVideo", kind = "framework")]
unsafe extern "C" {
    fn CVPixelBufferGetIOSurface(buffer: *const c_void) -> *const c_void;
    fn CVPixelBufferGetWidth(buffer: *const c_void) -> usize;
    fn CVPixelBufferGetHeight(buffer: *const c_void) -> usize;
}

// ─── Pixel format four-CCs ───────────────────────────────────────────────────

/// `'BGRA'` — `kCVPixelFormatType_32BGRA`. SDR fallback path.
const PIXEL_FORMAT_BGRA: u32 = 0x4247_5241;
/// `'RGhA'` — `kCVPixelFormatType_64RGBAHalf`. 16-bit float per channel,
/// the format SCK uses for HDR capture.
const PIXEL_FORMAT_RGBA_HALF: u32 = 0x5247_6841;

// ─── Delegate object for SCStreamOutput ──────────────────────────────────────

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = AllocAnyThread]
    #[name = "TVLEDStreamHandler"]
    #[derive(Debug)]
    pub struct StreamHandler;

    unsafe impl NSObjectProtocol for StreamHandler {}

    unsafe impl SCStreamOutput for StreamHandler {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        unsafe fn stream_did_output(
            &self,
            _stream: &SCStream,
            sample_buffer: &CMSampleBuffer,
            ty: SCStreamOutputType,
        ) {
            // SCStreamOutputType::Screen = 0. Audio (1) and microphone
            // (2) buffers also arrive on this queue if those are enabled
            // — we ignore them.
            if ty.0 != 0 {
                return;
            }
            if let Some(g) = GLOBAL.get() {
                unsafe { dispatch_frame(&g.state, sample_buffer) };
            }
        }
    }
);

unsafe fn dispatch_frame(state: &Arc<SharedState>, sample_buffer: &CMSampleBuffer) {
    let pb_ptr =
        unsafe { CMSampleBufferGetImageBuffer(sample_buffer as *const _ as *const c_void) };
    if pb_ptr.is_null() {
        return;
    }
    let surface = unsafe { CVPixelBufferGetIOSurface(pb_ptr) };
    if surface.is_null() {
        return;
    }
    let width = unsafe { CVPixelBufferGetWidth(pb_ptr) } as u32;
    let height = unsafe { CVPixelBufferGetHeight(pb_ptr) } as u32;
    crate::handle_capture_frame(state, surface, width, height);
}

// ─── Globals ─────────────────────────────────────────────────────────────────

struct CaptureGlobals {
    state: Arc<SharedState>,
    // Hold strong refs so SCK doesn't tear them down.
    _stream: Retained<SCStream>,
    _handler: Retained<StreamHandler>,
    _queue: dispatch2::DispatchRetained<DispatchQueue>,
    // SCStreamConfiguration.setColorSpaceName is documented as
    // unretained — SCK reads the pointer lazily during
    // serializeStreamProperties at startCapture time. If we let the NSString
    // drop after configure, SCK reads a dangling pointer and the process
    // crashes inside -[SCStream serializeStreamProperties]. Stash it.
    _color_space_name: Option<Retained<NSString>>,
}

// SCStream / our handler / dispatch queue are all thread-safe Objective-C
// objects; the Arc handles its own sync. We never mutate after init().
unsafe impl Send for CaptureGlobals {}
unsafe impl Sync for CaptureGlobals {}

static GLOBAL: OnceLock<CaptureGlobals> = OnceLock::new();

// ─── Public entry point ──────────────────────────────────────────────────────

/// Spin up a ScreenCaptureKit stream targeting `display_id`, `cap_w`×`cap_h`.
/// `hdr=true` requests 16-bit float, extendedLinearSRGB, HDR dynamic range.
/// Blocks until the stream's start completion handler fires (or 5s timeout).
pub fn start(
    display_id: u32,
    cap_w: u32,
    cap_h: u32,
    hdr: bool,
    state: Arc<SharedState>,
) -> Result<(), String> {
    let content = fetch_shareable_content()?;
    let displays = unsafe { content.displays() };
    let count = displays.len();
    let mut display_opt = None;
    for i in 0..count {
        let d = displays.objectAtIndex(i);
        if unsafe { d.displayID() } == display_id {
            display_opt = Some(d);
            break;
        }
    }
    let display = display_opt
        .ok_or_else(|| format!("Display id {display_id} not found via ScreenCaptureKit"))?;

    let filter = unsafe {
        SCContentFilter::initWithDisplay_excludingWindows(
            SCContentFilter::alloc(),
            &display,
            &NSArray::new(),
        )
    };

    let config = unsafe { SCStreamConfiguration::new() };
    let mut cs_name_keep: Option<Retained<NSString>> = None;
    unsafe {
        config.setWidth(cap_w as usize);
        config.setHeight(cap_h as usize);
        config.setShowsCursor(false);
        config.setQueueDepth(8);
        if hdr {
            config.setPixelFormat(PIXEL_FORMAT_RGBA_HALF);
            config.setCaptureDynamicRange(SCCaptureDynamicRange::HDRLocalDisplay);
            // Toll-free bridge NSString → CFString for setColorSpaceName.
            // extendedLinearSRGB gives us linear values potentially >1.0,
            // perfect for HDR tone-mapping in our shader.
            // CRITICAL: setColorSpaceName is unretained — SCK reads the
            // pointer lazily during startCapture's serializeStreamProperties.
            // Keep the NSString alive in CaptureGlobals.
            let cs_name = NSString::from_str("kCGColorSpaceExtendedLinearSRGB");
            let cs_cf: *const c_void = Retained::as_ptr(&cs_name).cast();
            let _: () = msg_send![&*config, setColorSpaceName: cs_cf];
            cs_name_keep = Some(cs_name);
        } else {
            config.setPixelFormat(PIXEL_FORMAT_BGRA);
            config.setCaptureDynamicRange(SCCaptureDynamicRange::SDR);
        }
    }

    let stream = unsafe {
        SCStream::initWithFilter_configuration_delegate(
            SCStream::alloc(),
            &filter,
            &config,
            None,
        )
    };
    let handler: Retained<StreamHandler> =
        unsafe { msg_send![StreamHandler::alloc(), init] };

    let queue = DispatchQueue::new("tv-led-mirror.sck.capture", None);

    let handler_proto = ProtocolObject::from_ref(&*handler);
    unsafe {
        stream
            .addStreamOutput_type_sampleHandlerQueue_error(
                handler_proto,
                SCStreamOutputType(0),
                Some(&queue),
            )
            .map_err(|e| format!("addStreamOutput failed: {}", ns_error_msg(&e)))?;
    }

    // Stash globals BEFORE starting capture so the delegate has somewhere to
    // route frames the moment the queue starts firing.
    let globals = CaptureGlobals {
        state,
        _stream: stream.clone(),
        _handler: handler,
        _queue: queue,
        _color_space_name: cs_name_keep,
    };
    if GLOBAL.set(globals).is_err() {
        return Err("capture::start called twice".into());
    }

    start_capture_sync(&stream)?;
    Ok(())
}

fn fetch_shareable_content() -> Result<Retained<SCShareableContent>, String> {
    use std::sync::mpsc;
    let (tx, rx) = mpsc::sync_channel::<Result<Retained<SCShareableContent>, String>>(1);
    let block = RcBlock::new(
        move |content_ptr: *mut SCShareableContent, error_ptr: *mut NSError| {
            let result = if !error_ptr.is_null() {
                let e = unsafe { &*error_ptr };
                Err(format!("SCShareableContent: {}", ns_error_msg(e)))
            } else if content_ptr.is_null() {
                Err("SCShareableContent returned null".into())
            } else {
                // The block borrows the pointer; we need to retain it.
                let content =
                    unsafe { Retained::retain(content_ptr) }.expect("nonnull retain");
                Ok(content)
            };
            let _ = tx.send(result);
        },
    );
    unsafe { SCShareableContent::getShareableContentWithCompletionHandler(&block) };
    rx.recv_timeout(Duration::from_secs(5))
        .map_err(|_| "SCShareableContent timed out (5s)".to_string())?
}

fn start_capture_sync(stream: &SCStream) -> Result<(), String> {
    use std::sync::mpsc;
    let (tx, rx) = mpsc::sync_channel::<Result<(), String>>(1);
    let block = RcBlock::new(move |error_ptr: *mut NSError| {
        let r = if error_ptr.is_null() {
            Ok(())
        } else {
            let e = unsafe { &*error_ptr };
            Err(format!("startCapture: {}", ns_error_msg(e)))
        };
        let _ = tx.send(r);
    });
    unsafe { stream.startCaptureWithCompletionHandler(Some(&block)) };
    rx.recv_timeout(Duration::from_secs(5))
        .map_err(|_| "startCapture timed out (5s)".to_string())?
}

fn ns_error_msg(e: &NSError) -> String {
    e.localizedDescription().to_string()
}
