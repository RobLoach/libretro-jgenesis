//! Safe wrappers around the callbacks that the frontend hands to the core.

use crate::ffi::{
    self, RetroAudioSampleBatchFloatFn, RetroAudioSampleBatchFn, RetroEnvironmentFn,
    RetroGameGeometry, RetroInputPollFn, RetroInputStateFn, RetroLogPrintfFn, RetroSystemAvInfo,
    RetroVideoRefreshFn,
};
use std::ffi::{CStr, CString, c_char, c_int, c_uint, c_void};
use std::path::PathBuf;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{PoisonError, RwLock};

#[derive(Clone, Copy)]
struct Callbacks {
    environment: Option<RetroEnvironmentFn>,
    video_refresh: Option<RetroVideoRefreshFn>,
    audio_sample_batch: Option<RetroAudioSampleBatchFn>,
    /// Negotiated per loaded game; when set, all audio goes through it instead of int16
    audio_sample_batch_float: Option<RetroAudioSampleBatchFloatFn>,
    input_poll: Option<RetroInputPollFn>,
    input_state: Option<RetroInputStateFn>,
    log: Option<RetroLogPrintfFn>,
}

static CALLBACKS: RwLock<Callbacks> = RwLock::new(Callbacks {
    environment: None,
    video_refresh: None,
    audio_sample_batch: None,
    audio_sample_batch_float: None,
    input_poll: None,
    input_state: None,
    log: None,
});

static SUPPORTS_INPUT_BITMASKS: AtomicBool = AtomicBool::new(false);
static CAN_DUPE: AtomicBool = AtomicBool::new(false);

fn callbacks() -> Callbacks {
    *CALLBACKS.read().unwrap_or_else(PoisonError::into_inner)
}

fn update_callbacks(f: impl FnOnce(&mut Callbacks)) {
    f(&mut CALLBACKS.write().unwrap_or_else(PoisonError::into_inner));
}

pub fn set_environment(cb: Option<RetroEnvironmentFn>) {
    update_callbacks(|callbacks| callbacks.environment = cb);
}

pub fn set_video_refresh(cb: Option<RetroVideoRefreshFn>) {
    update_callbacks(|callbacks| callbacks.video_refresh = cb);
}

pub fn set_audio_sample_batch(cb: Option<RetroAudioSampleBatchFn>) {
    update_callbacks(|callbacks| callbacks.audio_sample_batch = cb);
}

pub fn set_input_poll(cb: Option<RetroInputPollFn>) {
    update_callbacks(|callbacks| callbacks.input_poll = cb);
}

pub fn set_input_state(cb: Option<RetroInputStateFn>) {
    update_callbacks(|callbacks| callbacks.input_state = cb);
}

/// Call the environment callback.
///
/// # Safety
///
/// `data` must point to whatever type the frontend expects for `cmd`.
pub unsafe fn environment(cmd: c_uint, data: *mut c_void) -> bool {
    match callbacks().environment {
        // SAFETY: Guaranteed by caller
        Some(environment) => unsafe { environment(cmd, data) },
        None => false,
    }
}

/// Query the frontend's log interface, input bitmask extension, and frame duping support.
pub fn init_frontend_interfaces() {
    let mut log_callback = ffi::RetroLogCallback { log: None };
    // SAFETY: GET_LOG_INTERFACE takes a retro_log_callback*
    let has_log = unsafe {
        environment(ffi::RETRO_ENVIRONMENT_GET_LOG_INTERFACE, (&raw mut log_callback).cast())
    };
    if has_log {
        update_callbacks(|callbacks| callbacks.log = log_callback.log);
    }

    // SAFETY: GET_INPUT_BITMASKS takes no data (NULL is allowed)
    let bitmasks =
        unsafe { environment(ffi::RETRO_ENVIRONMENT_GET_INPUT_BITMASKS, ptr::null_mut()) };
    SUPPORTS_INPUT_BITMASKS.store(bitmasks, Ordering::Relaxed);

    let mut can_dupe = false;
    // SAFETY: GET_CAN_DUPE takes a bool*
    let ok =
        unsafe { environment(ffi::RETRO_ENVIRONMENT_GET_CAN_DUPE, (&raw mut can_dupe).cast()) };
    CAN_DUPE.store(ok && can_dupe, Ordering::Relaxed);

    Logger::install();
}

fn get_directory(cmd: c_uint) -> Option<PathBuf> {
    let mut dir: *const c_char = ptr::null();
    // SAFETY: GET_*_DIRECTORY takes a const char**
    let ok = unsafe { environment(cmd, (&raw mut dir).cast()) };
    if !ok || dir.is_null() {
        return None;
    }

    // SAFETY: Frontend returned a valid NUL-terminated string
    let dir = unsafe { CStr::from_ptr(dir) };
    let dir = dir.to_str().ok()?;
    (!dir.is_empty()).then(|| PathBuf::from(dir))
}

pub fn system_directory() -> Option<PathBuf> {
    get_directory(ffi::RETRO_ENVIRONMENT_GET_SYSTEM_DIRECTORY)
}

pub fn save_directory() -> Option<PathBuf> {
    get_directory(ffi::RETRO_ENVIRONMENT_GET_SAVE_DIRECTORY)
}

/// Save state size can grow during a session (see `Session::serialize`). Must be called from
/// `retro_init`.
pub fn set_serialization_quirks() {
    let mut quirks = ffi::RETRO_SERIALIZATION_QUIRK_CORE_VARIABLE_SIZE;
    // SAFETY: SET_SERIALIZATION_QUIRKS takes a uint64_t*
    let ok = unsafe {
        environment(ffi::RETRO_ENVIRONMENT_SET_SERIALIZATION_QUIRKS, (&raw mut quirks).cast())
    };
    if ok && quirks & ffi::RETRO_SERIALIZATION_QUIRK_FRONT_VARIABLE_SIZE == 0 {
        // Not a problem: Session pads the reported size so that it rarely needs to grow
        log::info!("Frontend does not support variable-size save states");
    }
}

/// Outputs the frontend wants for the current frame. Disabled outputs will be discarded, e.g.
/// for frames that run-ahead runs but never shows.
#[derive(Debug, Clone, Copy)]
pub struct AvEnable {
    pub video: bool,
    pub audio: bool,
}

pub fn audio_video_enable() -> AvEnable {
    let mut flags: c_int = 0;
    // SAFETY: GET_AUDIO_VIDEO_ENABLE takes an int* (retro_av_enable_flags)
    let ok = unsafe {
        environment(ffi::RETRO_ENVIRONMENT_GET_AUDIO_VIDEO_ENABLE, (&raw mut flags).cast())
    };
    if !ok {
        return AvEnable { video: true, audio: true };
    }

    AvEnable {
        video: flags & ffi::RETRO_AV_ENABLE_VIDEO != 0,
        audio: flags & ffi::RETRO_AV_ENABLE_AUDIO != 0,
    }
}

/// Content details from `GET_GAME_INFO_EXT`, copied out because the frontend's strings are only
/// valid during `retro_load_game`.
pub struct GameInfoExt {
    /// Path to the content file on disk; may be absent when the content is inside an archive
    pub full_path: Option<String>,
    /// Directory containing the content file, or the archive it is inside
    pub dir: Option<String>,
    /// Content name without extension (`RetroArch` uses the archive's name for archived content)
    pub name: Option<String>,
    /// Lowercase extension of the content file, without the dot
    pub ext: Option<String>,
}

fn owned_string(s: *const c_char) -> Option<String> {
    if s.is_null() {
        return None;
    }

    // SAFETY: Frontend strings are NUL-terminated
    let s = unsafe { CStr::from_ptr(s) }.to_str().ok()?;
    (!s.is_empty()).then(|| s.to_owned())
}

/// Extended info for the content being loaded. Only valid to call from `retro_load_game`.
pub fn game_info_ext() -> Option<GameInfoExt> {
    let mut info: *const ffi::RetroGameInfoExt = ptr::null();
    // SAFETY: GET_GAME_INFO_EXT takes a const retro_game_info_ext**
    let ok =
        unsafe { environment(ffi::RETRO_ENVIRONMENT_GET_GAME_INFO_EXT, (&raw mut info).cast()) };
    if !ok || info.is_null() {
        return None;
    }

    // SAFETY: Frontend returned a pointer to an array of retro_game_info_ext, whose first entry
    // is the loaded content
    let info = unsafe { &*info };
    Some(GameInfoExt {
        full_path: owned_string(info.full_path),
        dir: owned_string(info.dir),
        name: owned_string(info.name),
        ext: owned_string(info.ext),
    })
}

pub fn get_variable(key: &CStr) -> Option<String> {
    let mut variable = ffi::RetroVariable { key: key.as_ptr(), value: ptr::null() };
    // SAFETY: GET_VARIABLE takes a retro_variable*
    let ok =
        unsafe { environment(ffi::RETRO_ENVIRONMENT_GET_VARIABLE, (&raw mut variable).cast()) };
    if !ok || variable.value.is_null() {
        return None;
    }

    // SAFETY: Frontend returned a valid NUL-terminated string
    let value = unsafe { CStr::from_ptr(variable.value) };
    Some(value.to_string_lossy().into_owned())
}

pub fn variables_updated() -> bool {
    let mut updated = false;
    // SAFETY: GET_VARIABLE_UPDATE takes a bool*
    let ok = unsafe {
        environment(ffi::RETRO_ENVIRONMENT_GET_VARIABLE_UPDATE, (&raw mut updated).cast())
    };
    ok && updated
}

pub fn set_geometry(geometry: &RetroGameGeometry) {
    let mut geometry = *geometry;
    // SAFETY: SET_GEOMETRY takes a const retro_game_geometry*
    unsafe {
        environment(ffi::RETRO_ENVIRONMENT_SET_GEOMETRY, (&raw mut geometry).cast());
    }
}

pub fn set_system_av_info(av_info: &RetroSystemAvInfo) {
    let mut av_info = *av_info;
    // SAFETY: SET_SYSTEM_AV_INFO takes a const retro_system_av_info*
    unsafe {
        environment(ffi::RETRO_ENVIRONMENT_SET_SYSTEM_AV_INFO, (&raw mut av_info).cast());
    }
}

pub fn show_message(msg: &str, frames: c_uint) {
    let Ok(msg) = CString::new(msg) else { return };
    let mut message = ffi::RetroMessage { msg: msg.as_ptr(), frames };
    // SAFETY: SET_MESSAGE takes a const retro_message*
    unsafe {
        environment(ffi::RETRO_ENVIRONMENT_SET_MESSAGE, (&raw mut message).cast());
    }
}

/// Whether the frontend accepts a NULL frame to repeat the previous one. Callers must resend the
/// last frame instead when it doesn't.
pub fn can_dupe() -> bool {
    CAN_DUPE.load(Ordering::Relaxed)
}

/// Send a frame of XRGB8888 pixels to the frontend, or dupe the last frame if `frame` is None.
/// Dupes are dropped if the frontend doesn't support them (see [`can_dupe`]).
pub fn video_refresh(frame: Option<&[u32]>, width: u32, height: u32) {
    let Some(video_refresh) = callbacks().video_refresh else { return };
    if frame.is_none() && !can_dupe() {
        return;
    }

    let (data, pitch) = match frame {
        Some(frame) => (frame.as_ptr().cast::<c_void>(), width as usize * 4),
        None => (ptr::null(), 0),
    };

    // SAFETY: data is either NULL (dupe) or points to at least width*height u32 pixels
    unsafe {
        video_refresh(data, width, height, pitch);
    }
}

/// Use float audio for the loaded game if the frontend supports it. libretro requires this to be
/// decided once, during `retro_load_game`.
pub fn negotiate_float_audio() {
    let mut callback = ffi::RetroAudioSampleFloatCallback { batch: None };
    // SAFETY: GET_AUDIO_SAMPLE_BATCH_FLOAT takes a retro_audio_sample_float_callback*
    let ok = unsafe {
        environment(ffi::RETRO_ENVIRONMENT_GET_AUDIO_SAMPLE_BATCH_FLOAT, (&raw mut callback).cast())
    };
    let batch = if ok { callback.batch } else { None };
    update_callbacks(|callbacks| callbacks.audio_sample_batch_float = batch);
    if batch.is_some() {
        log::info!("Using float audio output");
    }
}

/// The float callback is only valid until `retro_unload_game`.
pub fn clear_float_audio() {
    update_callbacks(|callbacks| callbacks.audio_sample_batch_float = None);
}

/// The frontend's output sample rate, so the emulator can resample straight to it instead of the
/// frontend resampling a second time.
pub fn target_sample_rate() -> Option<u32> {
    let mut rate: c_uint = 0;
    // SAFETY: GET_TARGET_SAMPLE_RATE takes an unsigned*
    let ok = unsafe {
        environment(ffi::RETRO_ENVIRONMENT_GET_TARGET_SAMPLE_RATE, (&raw mut rate).cast())
    };
    (ok && (8_000..=384_000).contains(&rate)).then_some(rate)
}

/// Send interleaved stereo samples in [-1, 1] to the frontend: as float if negotiated, otherwise
/// converted to int16 in `scratch`.
pub fn audio_sample_batch(samples: &[f32], scratch: &mut Vec<i16>) {
    let callbacks = callbacks();

    if let Some(batch) = callbacks.audio_sample_batch_float {
        // SAFETY: Each chunk holds len / 2 interleaved stereo frames
        send_audio(samples, |chunk| unsafe { batch(chunk.as_ptr(), chunk.len() / 2) });
        return;
    }

    let Some(batch) = callbacks.audio_sample_batch else { return };
    scratch.clear();
    scratch.extend(samples.iter().map(|&sample| (sample * f32::from(i16::MAX)).round() as i16));
    // SAFETY: Each chunk holds len / 2 interleaved stereo frames
    send_audio(scratch, |chunk| unsafe { batch(chunk.as_ptr(), chunk.len() / 2) });
}

/// Frontends may accept fewer frames than offered; keep sending until done or stalled.
fn send_audio<T>(samples: &[T], mut send: impl FnMut(&[T]) -> usize) {
    let mut remaining = samples;
    while remaining.len() >= 2 {
        let written = send(remaining);
        if written == 0 {
            break;
        }
        remaining = &remaining[(2 * written).min(remaining.len())..];
    }
}

/// Number of ports the frontend currently has devices for, if it says.
pub fn input_max_users() -> Option<usize> {
    let mut users: c_uint = 0;
    // SAFETY: GET_INPUT_MAX_USERS takes an unsigned*
    let ok =
        unsafe { environment(ffi::RETRO_ENVIRONMENT_GET_INPUT_MAX_USERS, (&raw mut users).cast()) };
    ok.then_some(users as usize)
}

pub fn input_poll() {
    if let Some(input_poll) = callbacks().input_poll {
        // SAFETY: No arguments
        unsafe { input_poll() };
    }
}

/// Read the state of all joypad buttons for a port as a bitmask indexed by
/// `RETRO_DEVICE_ID_JOYPAD_*`.
pub fn joypad_buttons(port: c_uint) -> u16 {
    let Some(input_state) = callbacks().input_state else { return 0 };

    if SUPPORTS_INPUT_BITMASKS.load(Ordering::Relaxed) {
        // SAFETY: Plain function call
        let mask = unsafe {
            input_state(port, ffi::RETRO_DEVICE_JOYPAD, 0, ffi::RETRO_DEVICE_ID_JOYPAD_MASK)
        };
        return mask as u16;
    }

    (0..16).fold(0, |mask, id| {
        // SAFETY: Plain function call
        let pressed = unsafe { input_state(port, ffi::RETRO_DEVICE_JOYPAD, 0, id) } != 0;
        mask | (u16::from(pressed) << id)
    })
}

/// Routes the `log` crate to the frontend's log interface, falling back to stderr.
struct Logger;

static LOGGER: Logger = Logger;

impl Logger {
    fn install() {
        // Fails if a logger was already installed by an earlier retro_init; that's fine
        let _ = log::set_logger(&LOGGER);
        log::set_max_level(log::LevelFilter::Info);
    }
}

impl log::Log for Logger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= log::Level::Info
    }

    fn log(&self, record: &log::Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }

        let message = format!("[jgenesis] {}\n", record.args());

        let Some(log) = callbacks().log else {
            eprint!("{message}");
            return;
        };

        let level = match record.level() {
            log::Level::Error => ffi::RETRO_LOG_ERROR,
            log::Level::Warn => ffi::RETRO_LOG_WARN,
            log::Level::Info => ffi::RETRO_LOG_INFO,
            log::Level::Debug | log::Level::Trace => ffi::RETRO_LOG_DEBUG,
        };

        let Ok(message) = CString::new(message.replace('\0', "")) else { return };
        // SAFETY: Format string is "%s" with exactly one NUL-terminated string argument
        unsafe {
            log(level, c"%s".as_ptr(), message.as_ptr());
        }
    }

    fn flush(&self) {}
}
