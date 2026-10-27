//! Safe wrappers around the callbacks that the frontend hands to the core.

use crate::ffi::{
    self, RetroAudioSampleBatchFn, RetroEnvironmentFn, RetroGameGeometry, RetroInputPollFn,
    RetroInputStateFn, RetroLogPrintfFn, RetroSystemAvInfo, RetroVideoRefreshFn,
};
use std::ffi::{CStr, CString, c_char, c_uint, c_void};
use std::path::PathBuf;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{PoisonError, RwLock};

#[derive(Clone, Copy)]
struct Callbacks {
    environment: Option<RetroEnvironmentFn>,
    video_refresh: Option<RetroVideoRefreshFn>,
    audio_sample_batch: Option<RetroAudioSampleBatchFn>,
    input_poll: Option<RetroInputPollFn>,
    input_state: Option<RetroInputStateFn>,
    log: Option<RetroLogPrintfFn>,
}

static CALLBACKS: RwLock<Callbacks> = RwLock::new(Callbacks {
    environment: None,
    video_refresh: None,
    audio_sample_batch: None,
    input_poll: None,
    input_state: None,
    log: None,
});

static SUPPORTS_INPUT_BITMASKS: AtomicBool = AtomicBool::new(false);

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

/// Query the frontend's log interface and the input bitmask extension.
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

/// Send a frame of XRGB8888 pixels to the frontend, or dupe the last frame if `frame` is None.
pub fn video_refresh(frame: Option<&[u32]>, width: u32, height: u32) {
    let Some(video_refresh) = callbacks().video_refresh else { return };

    let (data, pitch) = match frame {
        Some(frame) => (frame.as_ptr().cast::<c_void>(), width as usize * 4),
        None => (ptr::null(), 0),
    };

    // SAFETY: data is either NULL (dupe) or points to at least width*height u32 pixels
    unsafe {
        video_refresh(data, width, height, pitch);
    }
}

/// Send interleaved stereo samples to the frontend.
pub fn audio_sample_batch(samples: &[i16]) {
    let Some(audio_sample_batch) = callbacks().audio_sample_batch else { return };

    let mut remaining = samples;
    while remaining.len() >= 2 {
        // SAFETY: remaining contains remaining.len() / 2 interleaved stereo frames
        let written = unsafe { audio_sample_batch(remaining.as_ptr(), remaining.len() / 2) };
        if written == 0 {
            break;
        }
        remaining = &remaining[(2 * written).min(remaining.len())..];
    }
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
