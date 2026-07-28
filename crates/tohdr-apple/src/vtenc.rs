//! HEVC plane encoding on the Apple Silicon media block, via VideoToolbox.
//!
//! Engine B's whole deficit is its plane encoder -- 23.3 of 30.8 CPU-seconds of a
//! 60 MP conversion sit in hpvca against ~0.1 ms in the muxer -- and tuning does
//! not close 8x against fixed-function silicon. So this replaces the encoder and
//! keeps the muxer. See docs/engine-comparison.md.
//!
//! Not via ImageIO: it tiles a 60 MP HEIC into a HEIF `grid`, and reassembling
//! tiles is a re-encode, not a remux, so `coded_image` refuses it
//! (`examples/probe_hw_planes.rs` fails on exactly that). VideoToolbox instead
//! hands back one coded frame plus parameter sets, which is what
//! [`tohdr_heif::CodedImage`] wants -- and the same shape Vulkan Video and D3D12
//! produce, so other backends slot in beside this one. Metal is not an option:
//! Apple exposes no encode API through it.
//!
//! The C entry points are declared here rather than taking three crates for
//! partial coverage: `objc2-core-video` 0.3.2 lacks
//! `CVPixelBufferGetBaseAddressOfPlane` and friends.
//!
//! VideoToolbox emits length-prefixed NALs and the `hvcC` atom verbatim, both
//! already the form HEIF stores, so neither is rewritten here.

use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Condvar, Mutex};

use objc2_core_foundation::{
    CFDictionary, CFNumber, CFNumberType, CFRetained, CFString, CFType, CFBoolean, CFData,
};
use tohdr_core::{GainPlane, Rgb};

use crate::{Error, Result};

// --- opaque CF-style handles ---
#[repr(C)]
struct OpaqueCVPixelBuffer(c_void);
#[repr(C)]
struct OpaqueVTCompressionSession(c_void);
#[repr(C)]
struct OpaqueCMSampleBuffer(c_void);
#[repr(C)]
struct OpaqueCMBlockBuffer(c_void);
#[repr(C)]
struct OpaqueCMFormatDescription(c_void);

/// CoreMedia's `CMTime`. Flags without `kCMTimeFlags_Valid` (bit 0) is
/// `kCMTimeInvalid`, which is what `CompleteFrames` takes to mean "all frames".
#[repr(C)]
#[derive(Clone, Copy)]
struct CMTime {
    value: i64,
    timescale: i32,
    flags: u32,
    epoch: i64,
}

impl CMTime {
    const VALID: u32 = 1;
    fn new(value: i64, timescale: i32) -> Self {
        CMTime { value, timescale, flags: Self::VALID, epoch: 0 }
    }
    fn invalid() -> Self {
        CMTime { value: 0, timescale: 0, flags: 0, epoch: 0 }
    }
}

type VTCompressionOutputCallback = extern "C-unwind" fn(
    output_callback_ref_con: *mut c_void,
    source_frame_ref_con: *mut c_void,
    status: i32,
    info_flags: u32,
    sample_buffer: *mut OpaqueCMSampleBuffer,
);

#[link(name = "CoreVideo", kind = "framework")]
unsafe extern "C" {
    fn CVPixelBufferCreate(
        allocator: *const c_void,
        width: usize,
        height: usize,
        pixel_format_type: u32,
        attributes: *const c_void,
        out: NonNull<*mut OpaqueCVPixelBuffer>,
    ) -> i32;
    fn CVPixelBufferLockBaseAddress(pb: *mut OpaqueCVPixelBuffer, flags: u64) -> i32;
    fn CVPixelBufferUnlockBaseAddress(pb: *mut OpaqueCVPixelBuffer, flags: u64) -> i32;
    fn CVPixelBufferGetBaseAddress(pb: *mut OpaqueCVPixelBuffer) -> *mut c_void;
    fn CVPixelBufferGetBytesPerRow(pb: *mut OpaqueCVPixelBuffer) -> usize;
    static kCVPixelBufferIOSurfacePropertiesKey: &'static CFString;
}

#[link(name = "CoreMedia", kind = "framework")]
unsafe extern "C" {
    fn CMSampleBufferGetDataBuffer(sb: *mut OpaqueCMSampleBuffer) -> *mut OpaqueCMBlockBuffer;
    fn CMSampleBufferGetFormatDescription(
        sb: *mut OpaqueCMSampleBuffer,
    ) -> *mut OpaqueCMFormatDescription;
    fn CMFormatDescriptionGetExtension(
        fd: *mut OpaqueCMFormatDescription,
        key: &CFString,
    ) -> *const CFType;
    fn CMBlockBufferGetDataLength(bb: *mut OpaqueCMBlockBuffer) -> usize;
    fn CMBlockBufferCopyDataBytes(
        bb: *mut OpaqueCMBlockBuffer,
        offset: usize,
        length: usize,
        dest: *mut c_void,
    ) -> i32;
    static kCMFormatDescriptionExtension_SampleDescriptionExtensionAtoms: &'static CFString;
}

#[link(name = "VideoToolbox", kind = "framework")]
unsafe extern "C" {
    fn VTCompressionSessionCreate(
        allocator: *const c_void,
        width: i32,
        height: i32,
        codec_type: u32,
        encoder_specification: *const c_void,
        source_image_buffer_attributes: *const c_void,
        compressed_data_allocator: *const c_void,
        output_callback: Option<VTCompressionOutputCallback>,
        output_callback_ref_con: *mut c_void,
        out: NonNull<*mut OpaqueVTCompressionSession>,
    ) -> i32;
    fn VTCompressionSessionEncodeFrame(
        session: *mut OpaqueVTCompressionSession,
        image_buffer: *mut OpaqueCVPixelBuffer,
        pts: CMTime,
        duration: CMTime,
        frame_properties: *const c_void,
        source_frame_refcon: *mut c_void,
        info_flags_out: *mut u32,
    ) -> i32;
    fn VTCompressionSessionCompleteFrames(
        session: *mut OpaqueVTCompressionSession,
        complete_until: CMTime,
    ) -> i32;
    fn VTCompressionSessionInvalidate(session: *mut OpaqueVTCompressionSession);
    fn VTSessionSetProperty(
        session: *mut OpaqueVTCompressionSession,
        key: &CFString,
        value: *const CFType,
    ) -> i32;
    static kVTCompressionPropertyKey_ProfileLevel: &'static CFString;
    static kVTCompressionPropertyKey_RealTime: &'static CFString;
    static kVTCompressionPropertyKey_AllowFrameReordering: &'static CFString;
    static kVTCompressionPropertyKey_MaxKeyFrameInterval: &'static CFString;
    static kVTCompressionPropertyKey_Quality: &'static CFString;
    static kVTProfileLevel_HEVC_Main_AutoLevel: &'static CFString;
    static kVTProfileLevel_HEVC_Monochrome_AutoLevel: &'static CFString;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(cf: *const c_void);
}

/// `'hvc1'` — HEVC.
const CODEC_HEVC: u32 = u32::from_be_bytes(*b"hvc1");
/// `'BGRA'` — 8-bit interleaved BGRA. Handing VideoToolbox packed RGB and
/// letting *it* convert to 4:2:0 is both faster and more correct than converting
/// in Rust first: Apple's path is the optimized one, and it applies the BT.709
/// matrix and range scaling that the `nclx` the muxer writes actually declares,
/// rather than a hand-rolled approximation of them.
const PF_BGRA: u32 = u32::from_be_bytes(*b"BGRA");
/// `'L008'` — one 8-bit luminance plane, the format Apple reports for every gain
/// map measured, and what the HEVC Monochrome profile codes natively.
const PF_L008: u32 = u32::from_be_bytes(*b"L008");

/// Whether to ask VideoToolbox for the low-latency path.
///
/// `true`: a single all-intra frame cannot pay off the quality path's multi-frame
/// analysis, so the 13-24% it saves is nearly free -- 0.17 dB at q85, nothing at
/// q100 (`examples/probe_vt_quality.rs`).
///
/// Judge this against the reconstruction, never against file size: fewer bytes at
/// the same requested quality is also what lower fidelity looks like.
const DEFAULT_REALTIME: bool = true;

/// One coded frame, in the form `tohdr_heif` stores it.
pub struct CodedPlane {
    pub width: u32,
    pub height: u32,
    pub monochrome: bool,
    /// `hvcC` box payload, excluding the box header.
    pub hvcc: Vec<u8>,
    /// Length-prefixed NAL units (4-byte big-endian lengths).
    pub data: Vec<u8>,
    /// Milliseconds acquiring the `VTCompressionSession` — creating and
    /// configuring one, or ~0 when [`session_reused`](CodedPlane::session_reused)
    /// says the pool had one.
    pub session_ms: f64,
    /// Milliseconds filling the pixel buffer on the CPU.
    pub fill_ms: f64,
    /// Milliseconds inside `EncodeFrame` + `CompleteFrames`.
    pub encode_ms: f64,
    /// Whether the session came from the pool rather than being created here.
    pub session_reused: bool,
}

/// What the output callback fills in.
///
/// The callback runs on VideoToolbox's own thread, but
/// `VTCompressionSessionCompleteFrames` returns only after every frame has been
/// emitted, so the read afterwards is ordered against the write — the completion
/// barrier is the synchronisation, and no lock is needed.
#[derive(Default)]
struct Sink {
    data: Option<Vec<u8>>,
    hvcc: Option<Vec<u8>>,
    status: i32,
}

impl Sink {
    /// Clear last frame's output so a pooled session cannot hand back stale
    /// bytes if the next encode silently produces nothing.
    fn reset(&mut self) {
        self.data = None;
        self.hvcc = None;
        self.status = 0;
    }
}

fn cf_i32(v: i32) -> CFRetained<CFNumber> {
    unsafe { CFNumber::new(None, CFNumberType::SInt32Type, &v as *const i32 as *const c_void) }
        .expect("CFNumberCreate never returns NULL for an SInt32")
}

fn cf_f32(v: f32) -> CFRetained<CFNumber> {
    unsafe { CFNumber::new(None, CFNumberType::Float32Type, &v as *const f32 as *const c_void) }
        .expect("CFNumberCreate never returns NULL for a Float32")
}

/// Pull the `hvcC` atom out of a sample's format description.
///
/// `SampleDescriptionExtensionAtoms` is keyed by atom fourcc and its `hvcC` value
/// is the box payload exactly as HEIF wants it. Reading it beats reassembling
/// VPS/SPS/PPS via `CMVideoFormatDescriptionGetHEVCParameterSetAtIndex`, which
/// would mean re-deriving the array structure, NAL-length size and profile flags
/// the atom already encodes correctly.
fn hvcc_from_sample(sb: *mut OpaqueCMSampleBuffer) -> Option<Vec<u8>> {
    let fd = unsafe { CMSampleBufferGetFormatDescription(sb) };
    if fd.is_null() {
        return None;
    }
    let atoms = unsafe {
        CMFormatDescriptionGetExtension(
            fd,
            kCMFormatDescriptionExtension_SampleDescriptionExtensionAtoms,
        )
    };
    if atoms.is_null() {
        return None;
    }
    let dict: &CFDictionary = unsafe { &*(atoms as *const CFDictionary) };
    let key = CFString::from_str("hvcC");
    let val = unsafe { dict.value(&*key as *const CFString as *const c_void) };
    if val.is_null() {
        return None;
    }
    let data: &CFData = unsafe { &*(val as *const CFData) };
    Some(data.to_vec())
}

/// HEVC NAL unit type for a prefix SEI.
const NAL_PREFIX_SEI: u8 = 39;
/// SEI payload type for `user_data_unregistered` (H.265 Table D.1).
const SEI_USER_DATA_UNREGISTERED: u8 = 5;

/// Drop VideoToolbox's private `user_data_unregistered` SEI from a
/// length-prefixed NAL stream.
///
/// Without this, identical pixels encode to different files: one byte per plane,
/// inside a 59-byte prefix SEI carrying what looks like an encode-time counter.
/// The coded slices are already byte-identical. Byte equality is how this project
/// checks a change did not alter output, so a run-to-run difference unrelated to
/// pixels makes that check useless.
///
/// Safe by definition -- `user_data_unregistered` carries no normative decoding
/// information (H.265 D.3.1). SEIs that do matter survive.
fn strip_unregistered_sei(data: Vec<u8>) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut off = 0usize;
    while off + 4 <= data.len() {
        let len = u32::from_be_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]]) as usize;
        let body = off + 4;
        // A length that runs past the end means this is not the stream we think
        // it is; copy the remainder verbatim rather than corrupting it.
        if len == 0 || body + len > data.len() {
            out.extend_from_slice(&data[off..]);
            return out;
        }
        if !is_unregistered_user_data_sei(&data[body..body + len]) {
            out.extend_from_slice(&data[off..body + len]);
        }
        off = body + len;
    }
    out.extend_from_slice(&data[off..]);
    out
}

/// Undo HEVC emulation prevention: a `00 00 03` triplet in the byte stream
/// stands for the two bytes `00 00`.
///
/// SEI payload sizes are counted in RBSP bytes, so the message walk has to run
/// over the unescaped form. Skipping this step is not a subtle inaccuracy — the
/// SEI we are looking for contains several `00 00 03` runs, so an escaped walk
/// lands mid-payload and rejects it.
fn unescape_rbsp(escaped: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(escaped.len());
    let mut i = 0;
    while i < escaped.len() {
        if i + 2 < escaped.len() && escaped[i] == 0 && escaped[i + 1] == 0 && escaped[i + 2] == 3 {
            out.extend_from_slice(&[0, 0]);
            i += 3;
        } else {
            out.push(escaped[i]);
            i += 1;
        }
    }
    out
}

/// Is this NAL a prefix SEI whose *only* message is `user_data_unregistered`?
///
/// The "only" matters: VideoToolbox packs one message per NAL here, and a NAL
/// mixing this with something normative must not be dropped wholesale.
fn is_unregistered_user_data_sei(nal: &[u8]) -> bool {
    // Two-byte NAL header; type is bits 1..7 of the first byte.
    if nal.len() < 3 || (nal[0] >> 1) & 0x3F != NAL_PREFIX_SEI {
        return false;
    }
    let rbsp = unescape_rbsp(&nal[2..]);
    let mut off = 0usize;
    let mut saw_unregistered = false;
    while off < rbsp.len() {
        // `0x80` is rbsp_trailing_bits, i.e. the end of the messages.
        if rbsp[off] == 0x80 {
            return saw_unregistered;
        }
        let mut payload_type = 0usize;
        while off < rbsp.len() && rbsp[off] == 0xFF {
            payload_type += 255;
            off += 1;
        }
        if off >= rbsp.len() {
            return false;
        }
        payload_type += rbsp[off] as usize;
        off += 1;
        let mut size = 0usize;
        while off < rbsp.len() && rbsp[off] == 0xFF {
            size += 255;
            off += 1;
        }
        if off >= rbsp.len() {
            return false;
        }
        size += rbsp[off] as usize;
        off += 1;
        if payload_type != SEI_USER_DATA_UNREGISTERED as usize {
            return false;
        }
        if off + size > rbsp.len() {
            // Truncated message: not something to make decisions on.
            return false;
        }
        saw_unregistered = true;
        off += size;
    }
    saw_unregistered
}

extern "C-unwind" fn on_output(
    refcon: *mut c_void,
    _source: *mut c_void,
    status: i32,
    _flags: u32,
    sample: *mut OpaqueCMSampleBuffer,
) {
    let sink = unsafe { &mut *(refcon as *mut Sink) };
    if status != 0 {
        sink.status = status;
        return;
    }
    if sample.is_null() {
        // Status 0 with no sample means the frame was dropped rather than
        // failed. Record it so the caller cannot read stale bytes as success.
        sink.status = -1;
        return;
    }
    let bb = unsafe { CMSampleBufferGetDataBuffer(sample) };
    if !bb.is_null() {
        let total = unsafe { CMBlockBufferGetDataLength(bb) };
        let mut out = vec![0u8; total];
        let st =
            unsafe { CMBlockBufferCopyDataBytes(bb, 0, total, out.as_mut_ptr() as *mut c_void) };
        // CopyDataBytes flattens a possibly non-contiguous buffer for us, so this
        // cannot silently truncate the way GetDataPointer's first-segment length
        // would.
        if st == 0 {
            sink.data = Some(strip_unregistered_sei(out));
        } else {
            sink.status = st;
            return;
        }
    }
    if sink.hvcc.is_none() {
        sink.hvcc = hvcc_from_sample(sample);
    }
}

/// Everything about a `VTCompressionSession` that is fixed once it exists.
///
/// Width, height and codec genuinely cannot change. Quality, `RealTime` and the
/// profile *could* be re-set on a live session, but are in the key deliberately:
/// a pooled session is then configured identically to a fresh one, so reuse cannot
/// quietly change what the encoder does. That is what makes
/// `examples/probe_vt_session_reuse.rs`'s byte-equality check meaningful.
///
/// The cost is that `--max-size`'s search misses the pool on each new quality it
/// tries; those sessions stay pooled, so a batch hits from the second file on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct SessionKey {
    width: u32,
    height: u32,
    mono: bool,
    /// Already clamped to 1..=100, so `q0` and `q1` share one session.
    quality: u8,
    realtime: bool,
}

impl SessionKey {
    /// Clamp `quality` the same way the property does, so two requests that
    /// configure an identical encoder share a session instead of each creating
    /// one.
    fn new(width: u32, height: u32, mono: bool, quality: u8, realtime: bool) -> Self {
        SessionKey {
            width,
            height,
            mono,
            quality: quality.clamp(1, 100),
            realtime,
        }
    }

    /// What this session costs against [`MAX_LIVE_PIXELS`].
    fn pixels(&self) -> u64 {
        self.width as u64 * self.height as u64
    }
}

/// A live `VTCompressionSession` and the sink its callback writes into.
///
/// Worth pooling because nothing about a session depends on the pixels, only on
/// [`SessionKey`]. At 12.19 MP the first base plane costs 97.1 ms and a pooled one
/// 27.0 (`examples/probe_vt_session_reuse.rs`).
///
/// Do not read that saving off `session_ms`: it sees only the 25.7 ms `Create`.
/// VideoToolbox brings the encoder up lazily on the first frame, so the other
/// ~44 ms hides inside `encode_ms` and looks like encoding.
struct Session {
    key: SessionKey,
    session: *mut OpaqueVTCompressionSession,
    /// The callback's `refcon`, from [`Box::into_raw`]. Raw rather than a live
    /// `Box` so this side and VideoToolbox's callback are not two aliasing
    /// references to one `Sink`, and so the address survives moves of this
    /// struct — which the pool does, on every checkout and check-in.
    sink: *mut Sink,
    /// Frames encoded so far, used to keep presentation timestamps distinct.
    /// A repeated PTS on one session is the kind of thing VideoToolbox is
    /// entitled to reject, and PTS is container metadata that appears nowhere in
    /// the coded bitstream, so numbering costs nothing.
    frames: i64,
}

// The pool hands sessions between threads: `MuxEngine::encode` runs the gain
// plane on a scoped thread that does not outlive the call, and `tohdr batch`
// runs whole files on worker threads. VideoToolbox permits `EncodeFrame` from
// any thread, and a checked-out `Session` is owned exclusively by whoever holds
// it, so there is no sharing to make unsound — only a move.
unsafe impl Send for Session {}

impl Session {
    fn create(key: SessionKey) -> Result<Self> {
        let sink = Box::into_raw(Box::new(Sink::default()));
        let mut session: *mut OpaqueVTCompressionSession = std::ptr::null_mut();
        let st = unsafe {
            VTCompressionSessionCreate(
                std::ptr::null(),
                key.width as i32,
                key.height as i32,
                CODEC_HEVC,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                Some(on_output),
                sink as *mut c_void,
                NonNull::from(&mut session),
            )
        };
        if st != 0 || session.is_null() {
            // Nothing owns the sink yet, so free it here rather than leaking it.
            drop(unsafe { Box::from_raw(sink) });
            return Err(Error::Unreadable(format!(
                "VTCompressionSessionCreate failed: {st}"
            )));
        }

        // All-intra, one self-contained frame per call: a gain-map HEIC item is a
        // still, so reordering and a keyframe interval above 1 are both
        // meaningless. That is also what makes reuse safe — every frame is an
        // IDR that refers to nothing before it.
        //
        // `realtime` is measured, not guessed. It is usually described as a
        // latency hint, but for a single still it selects how much analysis the
        // encoder does before committing, and that shows up in both time and
        // size.
        let profile = if key.mono {
            unsafe { kVTProfileLevel_HEVC_Monochrome_AutoLevel }
        } else {
            unsafe { kVTProfileLevel_HEVC_Main_AutoLevel }
        };
        let set = |k: &CFString, v: *const CFType| unsafe { VTSessionSetProperty(session, k, v) };
        set(
            unsafe { kVTCompressionPropertyKey_ProfileLevel },
            &**profile as *const CFType,
        );
        set(
            unsafe { kVTCompressionPropertyKey_RealTime },
            CFBoolean::new(key.realtime) as *const CFBoolean as *const CFType,
        );
        set(
            unsafe { kVTCompressionPropertyKey_AllowFrameReordering },
            CFBoolean::new(false) as *const CFBoolean as *const CFType,
        );
        let one = cf_i32(1);
        set(
            unsafe { kVTCompressionPropertyKey_MaxKeyFrameInterval },
            one.as_ref() as *const CFNumber as *const CFType,
        );
        // VideoToolbox's Quality is 0.0..=1.0, the same mapping ImageIO's
        // LossyCompressionQuality uses, so `quality` means the same thing on both
        // Apple paths.
        let q = cf_f32((key.quality as f32) / 100.0);
        set(
            unsafe { kVTCompressionPropertyKey_Quality },
            q.as_ref() as *const CFNumber as *const CFType,
        );

        Ok(Session { key, session, sink, frames: 0 })
    }

    /// Encode one frame and wait for it, returning `(data, hvcC)`.
    ///
    /// `CompleteFrames` is a flush, not a teardown, so the session is still
    /// usable afterwards — that is the whole basis for pooling. Only
    /// `Invalidate`, in [`Drop`], ends it.
    fn encode_frame(&mut self, pb: *mut OpaqueCVPixelBuffer) -> Result<(Vec<u8>, Vec<u8>)> {
        // Safe to touch: no frame is in flight, because the previous call did
        // not return until `CompleteFrames` had emitted everything.
        unsafe { &mut *self.sink }.reset();

        let pts = CMTime::new(self.frames * 20, 600);
        self.frames += 1;
        let mut flags = 0u32;
        let st = unsafe {
            VTCompressionSessionEncodeFrame(
                self.session,
                pb,
                pts,
                CMTime::new(20, 600),
                std::ptr::null(),
                std::ptr::null_mut(),
                &mut flags,
            )
        };
        if st != 0 {
            return Err(Error::Unreadable(format!("VTEncodeFrame failed: {st}")));
        }
        let st = unsafe { VTCompressionSessionCompleteFrames(self.session, CMTime::invalid()) };
        if st != 0 {
            return Err(Error::Unreadable(format!("VTCompleteFrames failed: {st}")));
        }

        let sink = unsafe { &mut *self.sink };
        if sink.status != 0 {
            // The live total goes in the message because this status arrives
            // *asynchronously* — `EncodeFrame` and `CompleteFrames` both
            // returned 0, so nothing at the call site refused the frame, and the
            // codes are undocumented. On this machine -17691 is what the media
            // block running out of resources looks like (see
            // `examples/probe_vt_limits.rs`), and the number that predicts it is
            // this one, not anything about the frame.
            return Err(Error::Unreadable(format!(
                "VideoToolbox encode callback reported {} with {:.0} MP of encoder sessions live \
                 (this frame {:.0} MP); --engine hpvca has no such limit",
                sink.status,
                live_session_pixels() as f64 / 1e6,
                self.key.pixels() as f64 / 1e6,
            )));
        }
        let data = sink
            .data
            .take()
            .ok_or(Error::NullFromFramework("VideoToolbox produced no frame"))?;
        // Re-read per frame rather than caching: `Sink::reset` clears it, so a
        // pooled session cannot hand back an `hvcC` that describes an earlier
        // frame's configuration.
        let hvcc = sink
            .hvcc
            .take()
            .ok_or(Error::NullFromFramework("VideoToolbox produced no hvcC"))?;
        Ok((data, hvcc))
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        unsafe {
            // Invalidate before freeing the sink: it tears down the encoder, so
            // no callback can still be holding the refcon afterwards.
            VTCompressionSessionInvalidate(self.session);
            CFRelease(self.session as *const c_void);
            drop(Box::from_raw(self.sink));
        }
    }
}

/// Idle sessions plus what every live session costs.
///
/// One lock over both, not two, because the accounting has to be atomic with the
/// eviction it drives: [`admit`] frees budget *by* ending idle sessions, so a
/// separate counter could be read after a session left the pool and before its
/// pixels were given back.
struct Pool {
    /// Sessions waiting for a matching [`SessionKey`].
    ///
    /// A `Vec` rather than a map because it holds single digits of entries and
    /// the key is five scalars: the linear scan is cheaper than hashing, and it
    /// keeps the "several sessions with the same key, one per concurrent job"
    /// case free — a map would need a bucket per key.
    idle: Vec<Session>,
    /// Pixels across every session that exists — idle *and* checked out.
    live_pixels: u64,
}

static POOL: Mutex<Pool> = Mutex::new(Pool { idle: Vec::new(), live_pixels: 0 });

/// Signalled whenever [`release`] or an eviction gives budget back.
static POOL_FREED: Condvar = Condvar::new();

/// Upper bound on *idle* sessions kept.
///
/// A leak guard, not a memory budget ([`MAX_LIVE_PIXELS`] is that): it stops a long
/// batch over many distinct *small* sizes accumulating one session each, which no
/// pixel budget would notice. 16 covers two orientations x two planes x four jobs;
/// beyond it the oldest idle session is dropped, degrading to create-per-call.
const MAX_IDLE_SESSIONS: usize = 16;

/// Pixels' worth of live `VTCompressionSession`s to permit at once.
///
/// The media block runs out and says so only *after* submission -- a negative
/// callback status, nothing to check up front -- so this gate is the only
/// defence. Not about one frame's size: 103.8 MP encodes fine, but at 60.2 MP the
/// 4th live session fails. Idle and in-flight count alike, hence a gate on
/// sessions that exist rather than a pool cap (`examples/probe_vt_limits.rs`).
///
/// 160 MP is 88% of the largest total verified good on the worst geometry
/// (3 x 60.2), margin against a boundary that is not exactly proportional to
/// pixels. It costs nothing real: one 60 MP file's two planes are 75 MP.
///
/// A frame larger than the whole budget is admitted when nothing else is live --
/// refusing to encode at all would be worse than letting the hardware refuse.
const MAX_LIVE_PIXELS: u64 = 160_000_000;

/// Whether to pool at all. On by default; `false` restores create-per-call, so
/// the two can be measured against each other in one process.
static SESSION_REUSE: AtomicBool = AtomicBool::new(true);

/// Sessions taken from the pool, and sessions created, since process start.
/// Read by `examples/probe_vt_session_reuse.rs`; cheap enough to leave on.
static POOL_HITS: AtomicU64 = AtomicU64::new(0);
static POOL_MISSES: AtomicU64 = AtomicU64::new(0);

/// Turn session pooling on or off, for measurement.
///
/// Turning it off also empties the pool, so a subsequent A/B run starts from the
/// same cold state as a fresh process.
pub fn set_session_reuse(on: bool) {
    SESSION_REUSE.store(on, Ordering::Relaxed);
    if !on {
        drain_session_pool();
    }
}

/// Drop every idle session, releasing its media-block resources.
pub fn drain_session_pool() {
    let mut pool = lock_pool();
    let drained: Vec<Session> = std::mem::take(&mut pool.idle);
    let freed: u64 = drained.iter().map(|s| s.key.pixels()).sum();
    pool.live_pixels = pool.live_pixels.saturating_sub(freed);
    // Still holding the lock: see `evict_oldest`.
    drop(drained);
    drop(pool);
    POOL_FREED.notify_all();
}

/// Pixels across every session that currently exists, idle or in flight.
///
/// Only for reporting — a decision made on this would be stale the moment the
/// lock is dropped, which is why [`admit`] does its arithmetic under the lock
/// instead of reading this.
pub fn live_session_pixels() -> u64 {
    lock_pool().live_pixels
}

/// How many sessions are idle. For tests: the pool's size is otherwise only
/// observable through timings.
#[cfg(test)]
fn idle_count() -> usize {
    lock_pool().idle.len()
}

/// `(hits, misses)` — sessions served from the pool, and sessions created.
pub fn session_pool_stats() -> (u64, u64) {
    (
        POOL_HITS.load(Ordering::Relaxed),
        POOL_MISSES.load(Ordering::Relaxed),
    )
}

/// A poisoned pool is not a reason to fail an encode: the only thing a panicking
/// holder can have left behind is a `Vec` of sessions that are individually
/// fine, and a `live_pixels` that is correct — every path that changes it holds
/// the lock across both the change and the session's fate.
fn lock_pool() -> std::sync::MutexGuard<'static, Pool> {
    POOL.lock().unwrap_or_else(|e| e.into_inner())
}

/// End the oldest idle session, giving its pixels back. Caller holds the lock.
///
/// The `Drop` runs here, under the lock, rather than being deferred to the
/// caller: `Invalidate` is what actually returns the resource to the media
/// block, so releasing the budget without it would let the next [`admit`]
/// proceed against hardware that is still full. It is a few milliseconds.
fn evict_oldest(pool: &mut Pool) -> bool {
    if pool.idle.is_empty() {
        return false;
    }
    let victim = pool.idle.remove(0);
    pool.live_pixels = pool.live_pixels.saturating_sub(victim.key.pixels());
    drop(victim);
    true
}

/// Reserve budget for a session of `pixels`, first by ending idle sessions and
/// then by waiting for a live one to finish.
///
/// Every successful call must be paired with [`release`], or with a
/// [`checkin`] that keeps the session alive and accounted for.
fn admit(pixels: u64) {
    let mut pool = lock_pool();
    loop {
        // `live_pixels == 0` admits a frame bigger than the whole budget rather
        // than deadlocking on one nothing can make room for.
        if pool.live_pixels == 0 || pool.live_pixels + pixels <= MAX_LIVE_PIXELS {
            pool.live_pixels += pixels;
            return;
        }
        if evict_oldest(&mut pool) {
            continue;
        }
        // Nothing idle left to end: every session in the budget is in flight on
        // another thread, so wait for one of them.
        pool = POOL_FREED
            .wait(pool)
            .unwrap_or_else(|e| e.into_inner());
    }
}

/// Give `pixels` back after a session has been dropped.
fn release(pixels: u64) {
    let mut pool = lock_pool();
    pool.live_pixels = pool.live_pixels.saturating_sub(pixels);
    drop(pool);
    POOL_FREED.notify_all();
}

/// Take an idle session matching `key`, if there is one.
///
/// A reused session keeps the reservation it was created with — it never left
/// the live set — so this deliberately does not touch `live_pixels`.
fn checkout(key: SessionKey) -> Option<Session> {
    if !SESSION_REUSE.load(Ordering::Relaxed) {
        return None;
    }
    let mut pool = lock_pool();
    let i = pool.idle.iter().position(|s| s.key == key)?;
    Some(pool.idle.swap_remove(i))
}

/// Return a session for the next caller, or end it if the pool is full.
fn checkin(session: Session) {
    if !SESSION_REUSE.load(Ordering::Relaxed) {
        let pixels = session.key.pixels();
        drop(session);
        release(pixels);
        return;
    }
    let mut pool = lock_pool();
    if pool.idle.len() >= MAX_IDLE_SESSIONS {
        evict_oldest(&mut pool);
    }
    pool.idle.push(session);
    drop(pool);
    // An eviction above freed budget, and a waiter cannot see that on its own.
    POOL_FREED.notify_all();
}

/// Encode one plane on the media block.
///
/// `mono` selects the HEVC Monochrome profile and an `L008` buffer, so a gain
/// plane stays single-channel end to end instead of acquiring neutral chroma.
fn encode_plane(
    width: u32,
    height: u32,
    mono: bool,
    quality: u8,
    realtime: bool,
    fill: impl FnOnce(*mut OpaqueCVPixelBuffer) -> Result<()>,
) -> Result<CodedPlane> {
    let pf = if mono { PF_L008 } else { PF_BGRA };

    // IOSurface-backed, or VideoToolbox may quietly choose a software encoder
    // and the whole point of this module is lost.
    let empty = CFDictionary::<CFString, CFType>::from_slices(&[], &[]);
    let attrs = CFDictionary::from_slices(
        &[unsafe { kCVPixelBufferIOSurfacePropertiesKey }],
        &[empty.as_opaque() as &CFType],
    );

    let mut pb: *mut OpaqueCVPixelBuffer = std::ptr::null_mut();
    let st = unsafe {
        CVPixelBufferCreate(
            std::ptr::null(),
            width as usize,
            height as usize,
            pf,
            attrs.as_opaque() as *const CFDictionary as *const c_void,
            NonNull::from(&mut pb),
        )
    };
    if st != 0 || pb.is_null() {
        return Err(Error::Unreadable(format!("CVPixelBufferCreate failed: {st}")));
    }
    // Owns `pb` from here; every early return below must release it.
    let guard = Releaser(pb as *const c_void);
    let t_fill = std::time::Instant::now();
    fill(pb)?;
    let fill_ms = t_fill.elapsed().as_secs_f64() * 1000.0;

    let key = SessionKey::new(width, height, mono, quality, realtime);
    let t_sess = std::time::Instant::now();
    let (mut session, session_reused) = match checkout(key) {
        Some(s) => {
            POOL_HITS.fetch_add(1, Ordering::Relaxed);
            (s, true)
        }
        None => {
            POOL_MISSES.fetch_add(1, Ordering::Relaxed);
            // Reserve before creating, and hand the reservation back if the
            // create fails — a session that never existed must not hold budget.
            admit(key.pixels());
            match Session::create(key) {
                Ok(s) => (s, false),
                Err(e) => {
                    release(key.pixels());
                    return Err(e);
                }
            }
        }
    };
    let session_ms = t_sess.elapsed().as_secs_f64() * 1000.0;

    let t_enc = std::time::Instant::now();
    // Not `?`: on failure `session` must be dropped rather than checked back in.
    // A session whose encode reported an error is not one to hand to the next
    // caller, and its `Drop` invalidates and releases it.
    let coded = session.encode_frame(pb);
    let encode_ms = t_enc.elapsed().as_secs_f64() * 1000.0;
    drop(guard);
    let (data, hvcc) = match coded {
        Ok(v) => v,
        Err(e) => {
            // Dropping the session is what returns its resources to the media
            // block, so the budget must not be given back before that — a
            // waiter admitted in between would find the hardware still full.
            drop(session);
            release(key.pixels());
            return Err(e);
        }
    };
    checkin(session);

    Ok(CodedPlane {
        width,
        height,
        monochrome: mono,
        hvcc,
        data,
        session_ms,
        fill_ms,
        encode_ms,
        session_reused,
    })
}

/// `CFRelease` on drop, so the early returns above cannot leak a pixel buffer.
struct Releaser(*const c_void);
impl Drop for Releaser {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0) };
        }
    }
}

/// Interleave 8-bit sRGB RGB into a BGRA buffer.
///
/// Deliberately *not* a colour conversion — see [`PF_BGRA`]. This is a byte
/// shuffle, so it is bounded by memory bandwidth rather than arithmetic, and
/// row-parallel because at 60 MP it still moves 241 MiB.
fn fill_bgra(pb: *mut OpaqueCVPixelBuffer, rgb: &Rgb) -> Result<()> {
    let (w, h) = (rgb.width as usize, rgb.height as usize);
    unsafe { CVPixelBufferLockBaseAddress(pb, 0) };
    let base = unsafe { CVPixelBufferGetBaseAddress(pb) } as *mut u8;
    let stride = unsafe { CVPixelBufferGetBytesPerRow(pb) };
    if base.is_null() {
        unsafe { CVPixelBufferUnlockBaseAddress(pb, 0) };
        return Err(Error::NullFromFramework("CVPixelBufferGetBaseAddress"));
    }

    // Raw pointers are not `Send`; each worker owns a disjoint row band, so
    // sharing the base is sound.
    struct Dst(*mut u8);
    unsafe impl Send for Dst {}
    unsafe impl Sync for Dst {}
    let dst = Dst(base);

    let src = &rgb.data;
    let workers = tohdr_core::par::threads().min(h.max(1));
    let per = h.div_ceil(workers.max(1));
    std::thread::scope(|s| {
        for wi in 0..workers {
            let y0 = wi * per;
            let y1 = ((wi + 1) * per).min(h);
            if y0 >= y1 {
                continue;
            }
            let dst = &dst;
            s.spawn(move || {
                for y in y0..y1 {
                    let row = unsafe { dst.0.add(y * stride) };
                    for x in 0..w {
                        let i = (y * w + x) * 3;
                        unsafe {
                            *row.add(x * 4) = src[i + 2] as u8; // B
                            *row.add(x * 4 + 1) = src[i + 1] as u8; // G
                            *row.add(x * 4 + 2) = src[i] as u8; // R
                            *row.add(x * 4 + 3) = 255; // A, ignored by the encoder
                        }
                    }
                }
            });
        }
    });
    unsafe { CVPixelBufferUnlockBaseAddress(pb, 0) };
    Ok(())
}

/// Copy an 8-bit gain plane into an `L008` buffer, row by row to respect stride.
fn fill_l008(pb: *mut OpaqueCVPixelBuffer, gain: &GainPlane) -> Result<()> {
    let (w, h) = (gain.width as usize, gain.height as usize);
    unsafe { CVPixelBufferLockBaseAddress(pb, 0) };
    let base = unsafe { CVPixelBufferGetBaseAddress(pb) } as *mut u8;
    let stride = unsafe { CVPixelBufferGetBytesPerRow(pb) };
    if base.is_null() {
        unsafe { CVPixelBufferUnlockBaseAddress(pb, 0) };
        return Err(Error::NullFromFramework("CVPixelBufferGetBaseAddress"));
    }
    for y in 0..h {
        unsafe {
            std::ptr::copy_nonoverlapping(gain.data[y * w..].as_ptr(), base.add(y * stride), w)
        };
    }
    unsafe { CVPixelBufferUnlockBaseAddress(pb, 0) };
    Ok(())
}

/// Encode the SDR base on the media block.
pub fn encode_base(base: &Rgb, quality: u8) -> Result<CodedPlane> {
    encode_base_tuned(base, quality, DEFAULT_REALTIME)
}

/// [`encode_base`] with the `RealTime` hint exposed, for measuring its effect.
pub fn encode_base_tuned(base: &Rgb, quality: u8, realtime: bool) -> Result<CodedPlane> {
    if base.bits != 8 {
        return Err(Error::Unreadable(format!(
            "the VideoToolbox base path takes 8-bit input, got {}-bit",
            base.bits
        )));
    }
    encode_plane(base.width, base.height, false, quality, realtime, |pb| {
        fill_bgra(pb, base)
    })
}

/// Encode the gain plane on the media block, as single-channel HEVC.
pub fn encode_gain(gain: &GainPlane, quality: u8) -> Result<CodedPlane> {
    encode_gain_tuned(gain, quality, DEFAULT_REALTIME)
}

/// [`encode_gain`] with the `RealTime` hint exposed, for measuring its effect.
pub fn encode_gain_tuned(gain: &GainPlane, quality: u8, realtime: bool) -> Result<CodedPlane> {
    encode_plane(gain.width, gain.height, true, quality, realtime, |pb| {
        fill_l008(pb, gain)
    })
}

impl CodedPlane {
    /// Hand the coded frame to the muxer, dropping the per-stage timings.
    fn into_coded_image(self) -> tohdr_heif::CodedImage {
        tohdr_heif::CodedImage {
            width: self.width,
            height: self.height,
            // Both paths here are 8-bit: `PF_BGRA` and `PF_L008` are 8-bit
            // formats, and `encode_base_tuned` rejects deeper input rather than
            // silently truncating it.
            bit_depth: 8,
            chroma: tohdr_heif::chroma_for(self.monochrome),
            hvcc: self.hvcc,
            data: self.data,
        }
    }
}

/// Engine B's hardware plane codec: the platform media block instead of a CPU
/// codec, with [`tohdr_heif::MuxEngine`] unchanged around it.
///
/// Faster than Engine A end to end (0.92x of it at `convert`'s default
/// subsampling, 0.98x at subsample 1; `docs/engine-comparison.md`), and ~9x
/// faster than [`tohdr_portable::HpvcaCodec`]. Its limits are the media block's:
/// 8-bit input only, and 4:2:0 chroma for the base regardless of `quality`, so a
/// caller that needs 4:4:4 or a deeper base wants the software codec.
#[derive(Debug, Default, Clone, Copy)]
pub struct VideoToolboxCodec;

impl tohdr_heif::PlaneCodec for VideoToolboxCodec {
    type Error = Error;

    fn name(&self) -> &'static str {
        "hardware-videotoolbox"
    }

    /// VideoToolbox converts the `PF_BGRA` buffer with the **BT.709** matrix, not
    /// the BT.601 the software codec uses.
    ///
    /// Measured on one hardware encode, varying only this declaration: BT.709
    /// reconstructs at 70.00 dB, BT.601 at 49.04 dB
    /// (`examples/probe_vt_colour.rs`). Declaring the wrong one cost 21 dB while
    /// making the file *smaller*, so it looked like a compression win rather than
    /// a colour bug — the reason `PlaneCodec::base_colour` has no default.
    fn base_colour(&self, primaries: tohdr_core::Primaries) -> tohdr_heif::ColourInfo {
        tohdr_heif::ColourInfo::Nclx {
            // The loader's decision, not VideoToolbox's: it converts whatever RGB
            // it is handed and never learns which primaries those were.
            primaries: primaries.nclx(),
            transfer: 13, // sRGB
            matrix: 1,    // BT.709
            full_range: true,
        }
    }

    fn encode_base(&self, base: &Rgb, quality: u8) -> Result<tohdr_heif::CodedImage> {
        Ok(encode_base(base, quality)?.into_coded_image())
    }

    fn encode_gain(&self, gain: &GainPlane, quality: u8) -> Result<tohdr_heif::CodedImage> {
        Ok(encode_gain(gain, quality)?.into_coded_image())
    }
}

impl VideoToolboxCodec {
    /// Can this codec take this base as it is?
    ///
    /// Checked up front so a caller can pick the software codec *instead of*
    /// starting a hardware encode and recovering from its error — the two
    /// produce different files, and deciding which one ran after the fact is
    /// worse than deciding before. Chroma is the caller's call, not a hard
    /// capability, so it is not checked here: this path is always 4:2:0.
    pub fn supports(base: &Rgb) -> core::result::Result<(), String> {
        if base.bits != 8 {
            return Err(format!(
                "the media block's base path is 8-bit, source is {}-bit",
                base.bits
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Length-prefix a NAL the way VideoToolbox and HEIF both do.
    fn prefixed(nal: &[u8]) -> Vec<u8> {
        let mut v = (nal.len() as u32).to_be_bytes().to_vec();
        v.extend_from_slice(nal);
        v
    }

    /// The exact SEI NAL VideoToolbox emitted for a 60 MP base plane, copied
    /// from the encoder's output. The `00 00 03` runs are what make the
    /// unescape step load-bearing rather than decorative.
    fn real_vt_sei() -> Vec<u8> {
        vec![
            0x4e, 0x01, 0x05, 0x32, 0x47, 0x56, 0x4a, 0xdc, 0x5c, 0x4c, 0x43, 0x3f, 0x94, 0xef,
            0xc5, 0x11, 0x3c, 0xd1, 0x43, 0xa8, 0x01, 0x00, 0x00, 0x03, 0x00, 0x03, 0x03, 0x00,
            0x00, 0x03, 0x00, 0x03, 0x02, 0x0c, 0x1d, 0x19, 0x00, 0x0b, 0x00, 0x00, 0x03, 0x00,
            0x00, 0x03, 0x00, 0x00, 0x5a, 0xd2, 0x0c, 0x03, 0x89, 0x24, 0x01, 0x0d, 0xff, 0xff,
            0xff, 0xff, 0x80,
        ]
    }

    /// An IDR slice NAL: type 20, so `(20 << 1) = 0x28` in the first header byte.
    fn slice_nal() -> Vec<u8> {
        vec![0x28, 0x01, 0xaf, 0x06, 0x1f, 0x00]
    }

    /// The pool and the budget are process-global, and cargo runs tests in
    /// parallel, so any test that observes either has to hold this. Without it
    /// they pass alone and fail together — the worst kind of test.
    static POOL_TESTS: Mutex<()> = Mutex::new(());

    fn pool_test_lock() -> std::sync::MutexGuard<'static, ()> {
        POOL_TESTS.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 64x64 keeps a real hardware encode to a few milliseconds. Not flat: a
    /// constant plane could compress to the same bytes for uninteresting reasons.
    fn tiny_gain() -> GainPlane {
        GainPlane {
            width: 64,
            height: 64,
            data: (0..64 * 64).map(|i| (i % 251) as u8).collect(),
        }
    }

    /// A runner VM has no usable media block: even 64x64 comes back -17691
    /// (`kVTSessionMalfunctionErr`) with nothing live. Probed once, so the tests
    /// below still run wherever the hardware is real.
    fn media_block_works() -> bool {
        static OK: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *OK.get_or_init(|| match encode_gain(&tiny_gain(), 85) {
            Ok(_) => {
                drain_session_pool();
                true
            }
            Err(e) => {
                eprintln!("skipping the VideoToolbox tests: {e}");
                false
            }
        })
    }

    #[test]
    fn recognises_the_real_videotoolbox_sei() {
        assert!(is_unregistered_user_data_sei(&real_vt_sei()));
    }

    #[test]
    fn unescape_restores_the_zero_pairs() {
        assert_eq!(unescape_rbsp(&[0x00, 0x00, 0x03, 0x01]), vec![0, 0, 1]);
        assert_eq!(unescape_rbsp(&[0x05, 0x32]), vec![0x05, 0x32]);
        // A `00 00 03` at the very end is still an escape, not literal data:
        // that is how a payload ending in `00 00` gets written (H.265 §7.4.1.1).
        assert_eq!(unescape_rbsp(&[0x01, 0x00, 0x00, 0x03]), vec![1, 0, 0]);
    }

    #[test]
    fn strips_the_sei_and_keeps_the_slice() {
        let mut stream = prefixed(&real_vt_sei());
        stream.extend(prefixed(&slice_nal()));
        let out = strip_unregistered_sei(stream);
        assert_eq!(out, prefixed(&slice_nal()));
    }

    /// The reason this exists: identical pixels must give identical bytes. The
    /// two captures below differ only in the vendor payload's varying byte.
    #[test]
    fn two_encodes_differing_only_in_the_sei_become_identical() {
        let mut a = prefixed(&real_vt_sei());
        a.extend(prefixed(&slice_nal()));
        let mut sei_b = real_vt_sei();
        sei_b[47] = 0xdc; // was 0xd2 — the byte that moved between runs
        let mut b = prefixed(&sei_b);
        b.extend(prefixed(&slice_nal()));
        assert_ne!(a, b);
        assert_eq!(strip_unregistered_sei(a), strip_unregistered_sei(b));
    }

    #[test]
    fn leaves_a_stream_without_the_sei_untouched() {
        let stream = prefixed(&slice_nal());
        assert_eq!(strip_unregistered_sei(stream.clone()), stream);
    }

    /// A prefix SEI carrying something normative must survive. Type 137 is
    /// mastering_display_colour_volume.
    #[test]
    fn keeps_other_sei_messages() {
        let mut sei = vec![0x4e, 0x01, 137, 0x02, 0xaa, 0xbb, 0x80];
        assert!(!is_unregistered_user_data_sei(&sei));
        let stream = prefixed(&sei);
        assert_eq!(strip_unregistered_sei(stream.clone()), stream);
        // ...and so must one that merely *starts* with the vendor message.
        sei = vec![0x4e, 0x01, 0x05, 0x01, 0xaa, 137, 0x01, 0xbb, 0x80];
        assert!(!is_unregistered_user_data_sei(&sei));
    }

    /// A garbled length must not panic or truncate the stream.
    #[test]
    fn absurd_length_passes_the_remainder_through() {
        let mut stream = vec![0xff, 0xff, 0xff, 0xff];
        stream.extend_from_slice(&[1, 2, 3]);
        assert_eq!(strip_unregistered_sei(stream.clone()), stream);
        assert_eq!(strip_unregistered_sei(Vec::new()), Vec::new());
        // A zero length is equally not a stream we can reason about.
        let zero = vec![0, 0, 0, 0, 9, 9];
        assert_eq!(strip_unregistered_sei(zero.clone()), zero);
    }

    /// The counterpart of `tohdr_portable`'s BT.601 assertion. A 21 dB
    /// regression is what a silent change here looks like, and it would show up
    /// as a *smaller* file — so it is pinned by a test rather than left to
    /// review.
    #[test]
    fn declares_the_bt709_matrix_videotoolbox_actually_writes() {
        for p in tohdr_core::Primaries::ALL {
            match tohdr_heif::PlaneCodec::base_colour(&VideoToolboxCodec, p) {
                tohdr_heif::ColourInfo::Nclx {
                    matrix,
                    primaries,
                    transfer,
                    ..
                } => {
                    assert_eq!(matrix, 1, "VideoToolbox writes BT.709; see probe_vt_colour.rs");
                    assert_eq!(transfer, 13);
                    // The BT.709 above is the *matrix* -- how VideoToolbox turned
                    // RGB into YCbCr -- and it must not move when the primaries do.
                    // Confusing the two is how a P3 file ends up declared 709.
                    assert_eq!(primaries, p.nclx(), "{p:?} was not passed through");
                }
                other => panic!("expected an nclx declaration, got {other:?}"),
            }
        }
    }

    #[test]
    fn truncated_nal_headers_are_not_seis() {
        assert!(!is_unregistered_user_data_sei(&[0x4e]));
        assert!(!is_unregistered_user_data_sei(&[0x4e, 0x01]));
        assert!(!is_unregistered_user_data_sei(&[]));
    }

    /// The key is what decides whether two encodes share a session, so what it
    /// does and does not distinguish is the whole contract.
    #[test]
    fn session_key_distinguishes_what_the_session_fixes() {
        let k = |w, h, mono, q, rt| SessionKey::new(w, h, mono, q, rt);
        let base = k(4032, 3024, false, 85, true);
        assert_eq!(base, k(4032, 3024, false, 85, true));
        // Geometry, plane kind, quality and RealTime each configure the encoder
        // differently, so none of them may be pooled across.
        assert_ne!(base, k(3024, 4032, false, 85, true), "orientation");
        assert_ne!(base, k(4032, 3024, true, 85, true), "mono vs colour");
        assert_ne!(base, k(4032, 3024, false, 100, true), "quality");
        assert_ne!(base, k(4032, 3024, false, 85, false), "RealTime");
    }

    /// Quality reaches VideoToolbox clamped, so the key must clamp identically
    /// or requests that configure the same encoder would each create a session.
    #[test]
    fn session_key_clamps_quality_the_way_the_property_does() {
        assert_eq!(
            SessionKey::new(8, 8, false, 0, true),
            SessionKey::new(8, 8, false, 1, true)
        );
        assert_eq!(SessionKey::new(8, 8, false, 200, true).quality, 100);
    }

    /// The property the whole pool rests on: a reused session must encode
    /// identically to a fresh one, or a file's bytes would depend on its
    /// position in a batch.
    ///
    /// This test owns the VideoToolbox boundary deliberately. There is no way to
    /// check the claim without a real encoder, and a probe that has to be run by
    /// hand is not a regression test — a future change to `Session` that quietly
    /// broke byte-transparency would otherwise be caught by nobody. 64x64
    /// monochrome keeps it to a few milliseconds.
    #[test]
    fn a_pooled_session_encodes_identically_to_a_fresh_one() {
        let _serial = pool_test_lock();
        if !media_block_works() {
            return;
        }
        let gain = tiny_gain();

        // Two cold encodes, which is what the recorded output hashes were taken
        // under.
        set_session_reuse(false);
        let cold = encode_gain(&gain, 85).expect("cold encode");
        assert!(!cold.session_reused);

        set_session_reuse(true);
        drain_session_pool();
        let miss = encode_gain(&gain, 85).expect("first pooled encode");
        let hit = encode_gain(&gain, 85).expect("second pooled encode");
        assert!(!miss.session_reused, "a drained pool cannot hit");
        assert!(hit.session_reused, "the second encode should come from the pool");

        assert_eq!(hit.data, cold.data, "reuse changed the coded bitstream");
        assert_eq!(miss.data, cold.data, "pooling changed even the first encode");
        assert_eq!(hit.hvcc, cold.hvcc, "reuse changed the hvcC");

        // A different quality is a different key, so it must not be served from
        // the pool — and must produce different bytes, which is the cheap way to
        // notice a session that ignored the new setting.
        let other = encode_gain(&gain, 40).expect("encode at another quality");
        assert!(!other.session_reused, "q40 must not reuse the q85 session");
        assert_ne!(other.data, cold.data, "q40 encoded the same as q85");

        drain_session_pool();
    }

    /// The budget only means anything if it is given back. Every path that ends a
    /// session releases by hand — there is no `Drop` doing it, because eviction
    /// runs while the pool lock is held and a reentrant release would deadlock —
    /// so an unbalanced pair is a real possibility and a leak here would
    /// eventually wedge [`admit`] on a budget nothing can free.
    #[test]
    fn a_completed_encode_leaves_no_budget_reserved() {
        let _serial = pool_test_lock();
        if !media_block_works() {
            return;
        }
        drain_session_pool();
        assert_eq!(live_session_pixels(), 0, "drain must release everything");

        let gain = tiny_gain();
        encode_gain(&gain, 85).expect("encode");
        assert_eq!(
            live_session_pixels(),
            64 * 64,
            "the pooled session still holds its reservation"
        );
        drain_session_pool();
        assert_eq!(live_session_pixels(), 0);

        // A failing encode must release too. 8-bit is the only base depth the
        // hardware path takes, so a 16-bit one is rejected before any session
        // exists -- which is exactly the path that could leak a reservation.
        let deep = Rgb { width: 64, height: 64, bits: 16, data: vec![0; 64 * 64 * 3] };
        assert!(encode_base(&deep, 85).is_err());
        assert_eq!(live_session_pixels(), 0, "a failed encode leaked budget");
    }

    /// Idle sessions hold the media block's resources exactly as in-flight ones
    /// do, so making room means *ending* them, not merely not adding more.
    #[test]
    fn admitting_a_large_frame_ends_idle_sessions_to_make_room() {
        let _serial = pool_test_lock();
        if !media_block_works() {
            return;
        }
        set_session_reuse(true);
        drain_session_pool();

        let gain = tiny_gain();
        encode_gain(&gain, 85).expect("encode");
        encode_gain(&gain, 40).expect("encode at another quality");
        assert_eq!(idle_count(), 2, "two distinct qualities, two idle sessions");

        // Ask for the whole budget: the only way to grant it is to end both.
        admit(MAX_LIVE_PIXELS);
        assert_eq!(idle_count(), 0, "idle sessions were kept while a frame waited");
        assert_eq!(live_session_pixels(), MAX_LIVE_PIXELS);
        release(MAX_LIVE_PIXELS);
        assert_eq!(live_session_pixels(), 0);
    }

    /// A frame larger than the entire budget must still encode when nothing else
    /// is live. The alternative is a limit that refuses work the hardware would
    /// have accepted — a single 103.8 MP frame encodes fine (`probe_vt_limits`).
    #[test]
    fn a_frame_bigger_than_the_budget_is_admitted_when_alone() {
        let _serial = pool_test_lock();
        drain_session_pool();
        admit(MAX_LIVE_PIXELS * 2);
        assert_eq!(live_session_pixels(), MAX_LIVE_PIXELS * 2);
        release(MAX_LIVE_PIXELS * 2);
        assert_eq!(live_session_pixels(), 0);
    }

    /// With the budget full and nothing idle to end, a waiter has to be woken by
    /// the release rather than spin or fail. If the notify were missing this test
    /// would hang instead of failing, which is the honest signal: the bug it
    /// guards against is a lost wakeup.
    #[test]
    fn a_waiter_is_admitted_once_a_live_session_finishes() {
        let _serial = pool_test_lock();
        drain_session_pool();
        admit(MAX_LIVE_PIXELS);

        let waiter = std::thread::spawn(|| {
            admit(MAX_LIVE_PIXELS);
            release(MAX_LIVE_PIXELS);
        });
        // The thread cannot get past `admit` until this runs: the budget is full
        // and the pool is empty, so there is nothing for it to evict.
        release(MAX_LIVE_PIXELS);
        waiter.join().expect("waiter thread");
        assert_eq!(live_session_pixels(), 0);
    }
}
