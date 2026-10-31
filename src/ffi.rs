//! Hand-written subset of `libretro.h`.
//!
//! Only the types, constants, and callbacks used by this core are declared. Layouts must match
//! <https://github.com/libretro/libretro-common/blob/master/include/libretro.h> exactly.

use std::ffi::{c_char, c_int, c_uint, c_void};

pub const RETRO_API_VERSION: c_uint = 1;

pub const RETRO_DEVICE_NONE: c_uint = 0;
pub const RETRO_DEVICE_JOYPAD: c_uint = 1;

pub const RETRO_DEVICE_ID_JOYPAD_B: c_uint = 0;
pub const RETRO_DEVICE_ID_JOYPAD_Y: c_uint = 1;
pub const RETRO_DEVICE_ID_JOYPAD_SELECT: c_uint = 2;
pub const RETRO_DEVICE_ID_JOYPAD_START: c_uint = 3;
pub const RETRO_DEVICE_ID_JOYPAD_UP: c_uint = 4;
pub const RETRO_DEVICE_ID_JOYPAD_DOWN: c_uint = 5;
pub const RETRO_DEVICE_ID_JOYPAD_LEFT: c_uint = 6;
pub const RETRO_DEVICE_ID_JOYPAD_RIGHT: c_uint = 7;
pub const RETRO_DEVICE_ID_JOYPAD_A: c_uint = 8;
pub const RETRO_DEVICE_ID_JOYPAD_X: c_uint = 9;
pub const RETRO_DEVICE_ID_JOYPAD_L: c_uint = 10;
pub const RETRO_DEVICE_ID_JOYPAD_R: c_uint = 11;
pub const RETRO_DEVICE_ID_JOYPAD_MASK: c_uint = 256;

/// Subclass of `RETRO_DEVICE_JOYPAD`, used for e.g. "6-button" controller types.
#[must_use]
pub const fn retro_device_subclass(base: c_uint, id: c_uint) -> c_uint {
    ((id + 1) << 8) | base
}

pub const RETRO_REGION_NTSC: c_uint = 0;
pub const RETRO_REGION_PAL: c_uint = 1;

pub const RETRO_ENVIRONMENT_EXPERIMENTAL: c_uint = 0x10000;
pub const RETRO_ENVIRONMENT_GET_CAN_DUPE: c_uint = 3;
pub const RETRO_ENVIRONMENT_SET_MESSAGE: c_uint = 6;
pub const RETRO_ENVIRONMENT_GET_SYSTEM_DIRECTORY: c_uint = 9;
pub const RETRO_ENVIRONMENT_SET_PIXEL_FORMAT: c_uint = 10;
pub const RETRO_ENVIRONMENT_SET_INPUT_DESCRIPTORS: c_uint = 11;
pub const RETRO_ENVIRONMENT_SET_DISK_CONTROL_INTERFACE: c_uint = 13;
pub const RETRO_ENVIRONMENT_GET_VARIABLE: c_uint = 15;
pub const RETRO_ENVIRONMENT_SET_VARIABLES: c_uint = 16;
pub const RETRO_ENVIRONMENT_GET_VARIABLE_UPDATE: c_uint = 17;
pub const RETRO_ENVIRONMENT_GET_LOG_INTERFACE: c_uint = 27;
pub const RETRO_ENVIRONMENT_GET_SAVE_DIRECTORY: c_uint = 31;
pub const RETRO_ENVIRONMENT_SET_SYSTEM_AV_INFO: c_uint = 32;
pub const RETRO_ENVIRONMENT_SET_SUBSYSTEM_INFO: c_uint = 34;
pub const RETRO_ENVIRONMENT_SET_CONTROLLER_INFO: c_uint = 35;
pub const RETRO_ENVIRONMENT_SET_GEOMETRY: c_uint = 37;
pub const RETRO_ENVIRONMENT_SET_SERIALIZATION_QUIRKS: c_uint = 44;
pub const RETRO_ENVIRONMENT_GET_VFS_INTERFACE: c_uint = 0x2D | RETRO_ENVIRONMENT_EXPERIMENTAL;
pub const RETRO_ENVIRONMENT_GET_AUDIO_VIDEO_ENABLE: c_uint = 0x2F | RETRO_ENVIRONMENT_EXPERIMENTAL;
pub const RETRO_ENVIRONMENT_GET_INPUT_BITMASKS: c_uint = 0x33 | RETRO_ENVIRONMENT_EXPERIMENTAL;
pub const RETRO_ENVIRONMENT_GET_CORE_OPTIONS_VERSION: c_uint = 52;
pub const RETRO_ENVIRONMENT_GET_INPUT_MAX_USERS: c_uint = 61;
pub const RETRO_ENVIRONMENT_SET_CONTENT_INFO_OVERRIDE: c_uint = 65;
pub const RETRO_ENVIRONMENT_GET_GAME_INFO_EXT: c_uint = 66;
pub const RETRO_ENVIRONMENT_SET_CORE_OPTIONS_V2: c_uint = 67;
pub const RETRO_ENVIRONMENT_GET_TARGET_SAMPLE_RATE: c_uint = 0x51 | RETRO_ENVIRONMENT_EXPERIMENTAL;
pub const RETRO_ENVIRONMENT_GET_AUDIO_SAMPLE_BATCH_FLOAT: c_uint =
    0x55 | RETRO_ENVIRONMENT_EXPERIMENTAL;
pub const RETRO_ENVIRONMENT_GET_MEMORY_STATUS: c_uint = 0x56 | RETRO_ENVIRONMENT_EXPERIMENTAL;

pub const RETRO_SERIALIZATION_QUIRK_CORE_VARIABLE_SIZE: u64 = 1 << 2;
pub const RETRO_SERIALIZATION_QUIRK_FRONT_VARIABLE_SIZE: u64 = 1 << 3;

pub const RETRO_AV_ENABLE_VIDEO: c_int = 1 << 0;
pub const RETRO_AV_ENABLE_AUDIO: c_int = 1 << 1;

pub const RETRO_VFS_FILE_ACCESS_READ: c_uint = 1 << 0;
pub const RETRO_VFS_FILE_ACCESS_WRITE: c_uint = 1 << 1;
pub const RETRO_VFS_FILE_ACCESS_HINT_NONE: c_uint = 0;

pub const RETRO_VFS_SEEK_POSITION_START: c_int = 0;
pub const RETRO_VFS_SEEK_POSITION_CURRENT: c_int = 1;
pub const RETRO_VFS_SEEK_POSITION_END: c_int = 2;

pub const RETRO_VFS_STAT_IS_VALID: c_int = 1 << 0;

/// Lowest VFS interface version with `stat` and `mkdir`
pub const RETRO_VFS_INTERFACE_VERSION_3: u32 = 3;

pub const RETRO_PIXEL_FORMAT_XRGB8888: c_int = 1;

pub const RETRO_LOG_DEBUG: c_int = 0;
pub const RETRO_LOG_INFO: c_int = 1;
pub const RETRO_LOG_WARN: c_int = 2;
pub const RETRO_LOG_ERROR: c_int = 3;

pub const RETRO_NUM_CORE_OPTION_VALUES_MAX: usize = 128;

pub type RetroEnvironmentFn = unsafe extern "C" fn(cmd: c_uint, data: *mut c_void) -> bool;
pub type RetroVideoRefreshFn =
    unsafe extern "C" fn(data: *const c_void, width: c_uint, height: c_uint, pitch: usize);
pub type RetroAudioSampleFn = unsafe extern "C" fn(left: i16, right: i16);
pub type RetroAudioSampleBatchFn = unsafe extern "C" fn(data: *const i16, frames: usize) -> usize;
pub type RetroAudioSampleBatchFloatFn =
    unsafe extern "C" fn(data: *const f32, frames: usize) -> usize;
pub type RetroInputPollFn = unsafe extern "C" fn();
pub type RetroInputStateFn =
    unsafe extern "C" fn(port: c_uint, device: c_uint, index: c_uint, id: c_uint) -> i16;
pub type RetroLogPrintfFn = unsafe extern "C" fn(level: c_int, fmt: *const c_char, ...);

#[repr(C)]
pub struct RetroSystemInfo {
    pub library_name: *const c_char,
    pub library_version: *const c_char,
    pub valid_extensions: *const c_char,
    pub need_fullpath: bool,
    pub block_extract: bool,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RetroGameGeometry {
    pub base_width: c_uint,
    pub base_height: c_uint,
    pub max_width: c_uint,
    pub max_height: c_uint,
    pub aspect_ratio: f32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RetroSystemTiming {
    pub fps: f64,
    pub sample_rate: f64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RetroSystemAvInfo {
    pub geometry: RetroGameGeometry,
    pub timing: RetroSystemTiming,
}

#[repr(C)]
pub struct RetroGameInfo {
    pub path: *const c_char,
    pub data: *const c_void,
    pub size: usize,
    pub meta: *const c_char,
}

#[repr(C)]
pub struct RetroGameInfoExt {
    pub full_path: *const c_char,
    pub archive_path: *const c_char,
    pub archive_file: *const c_char,
    pub dir: *const c_char,
    pub name: *const c_char,
    pub ext: *const c_char,
    pub meta: *const c_char,
    pub data: *const c_void,
    pub size: usize,
    pub file_in_archive: bool,
    pub persistent_data: bool,
}

#[repr(C)]
pub struct RetroAudioSampleFloatCallback {
    pub batch: Option<RetroAudioSampleBatchFloatFn>,
}

#[repr(C)]
pub struct RetroMemoryStatus {
    pub free: u64,
    pub total: u64,
}

#[repr(C)]
pub struct RetroVariable {
    pub key: *const c_char,
    pub value: *const c_char,
}

#[repr(C)]
pub struct RetroMessage {
    pub msg: *const c_char,
    pub frames: c_uint,
}

#[repr(C)]
pub struct RetroInputDescriptor {
    pub port: c_uint,
    pub device: c_uint,
    pub index: c_uint,
    pub id: c_uint,
    pub description: *const c_char,
}

#[repr(C)]
pub struct RetroControllerDescription {
    pub desc: *const c_char,
    pub id: c_uint,
}

#[repr(C)]
pub struct RetroControllerInfo {
    pub types: *const RetroControllerDescription,
    pub num_types: c_uint,
}

#[repr(C)]
pub struct RetroLogCallback {
    pub log: Option<RetroLogPrintfFn>,
}

#[repr(C)]
pub struct RetroSystemContentInfoOverride {
    pub extensions: *const c_char,
    pub need_fullpath: bool,
    pub persistent_data: bool,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct RetroCoreOptionValue {
    pub value: *const c_char,
    pub label: *const c_char,
}

#[repr(C)]
pub struct RetroCoreOptionV2Category {
    pub key: *const c_char,
    pub desc: *const c_char,
    pub info: *const c_char,
}

#[repr(C)]
pub struct RetroCoreOptionV2Definition {
    pub key: *const c_char,
    pub desc: *const c_char,
    pub desc_categorized: *const c_char,
    pub info: *const c_char,
    pub info_categorized: *const c_char,
    pub category_key: *const c_char,
    pub values: [RetroCoreOptionValue; RETRO_NUM_CORE_OPTION_VALUES_MAX],
    pub default_value: *const c_char,
}

#[repr(C)]
pub struct RetroCoreOptionsV2 {
    pub categories: *const RetroCoreOptionV2Category,
    pub definitions: *const RetroCoreOptionV2Definition,
}

pub type RetroSetEjectStateFn = unsafe extern "C" fn(ejected: bool) -> bool;
pub type RetroGetEjectStateFn = unsafe extern "C" fn() -> bool;
pub type RetroGetImageIndexFn = unsafe extern "C" fn() -> c_uint;
pub type RetroSetImageIndexFn = unsafe extern "C" fn(index: c_uint) -> bool;
pub type RetroGetNumImagesFn = unsafe extern "C" fn() -> c_uint;
pub type RetroReplaceImageIndexFn =
    unsafe extern "C" fn(index: c_uint, info: *const RetroGameInfo) -> bool;
pub type RetroAddImageIndexFn = unsafe extern "C" fn() -> bool;

/// Opaque frontend file handle
#[repr(C)]
pub struct RetroVfsFileHandle {
    _private: [u8; 0],
}

type VfsPathFn = unsafe extern "C" fn(path: *const c_char) -> c_int;

/// VFS API v1-v3. Later versions append fields, which this core never reads. Directory functions
/// are unused, so only their size matters.
#[repr(C)]
pub struct RetroVfsInterface {
    pub get_path: Option<unsafe extern "C" fn(*mut RetroVfsFileHandle) -> *const c_char>,
    pub open: Option<
        unsafe extern "C" fn(
            path: *const c_char,
            mode: c_uint,
            hints: c_uint,
        ) -> *mut RetroVfsFileHandle,
    >,
    pub close: Option<unsafe extern "C" fn(*mut RetroVfsFileHandle) -> c_int>,
    pub size: Option<unsafe extern "C" fn(*mut RetroVfsFileHandle) -> i64>,
    pub tell: Option<unsafe extern "C" fn(*mut RetroVfsFileHandle) -> i64>,
    pub seek: Option<
        unsafe extern "C" fn(*mut RetroVfsFileHandle, offset: i64, seek_position: c_int) -> i64,
    >,
    pub read: Option<unsafe extern "C" fn(*mut RetroVfsFileHandle, *mut c_void, u64) -> i64>,
    pub write: Option<unsafe extern "C" fn(*mut RetroVfsFileHandle, *const c_void, u64) -> i64>,
    pub flush: Option<unsafe extern "C" fn(*mut RetroVfsFileHandle) -> c_int>,
    pub remove: Option<VfsPathFn>,
    pub rename: Option<unsafe extern "C" fn(old: *const c_char, new: *const c_char) -> c_int>,
    // v2
    pub truncate: Option<unsafe extern "C" fn(*mut RetroVfsFileHandle, i64) -> i64>,
    // v3
    pub stat: Option<unsafe extern "C" fn(path: *const c_char, size: *mut i32) -> c_int>,
    pub mkdir: Option<VfsPathFn>,
    pub opendir: Option<unsafe extern "C" fn()>,
    pub readdir: Option<unsafe extern "C" fn()>,
    pub dirent_get_name: Option<unsafe extern "C" fn()>,
    pub dirent_is_dir: Option<unsafe extern "C" fn()>,
    pub closedir: Option<unsafe extern "C" fn()>,
}

#[repr(C)]
pub struct RetroVfsInterfaceInfo {
    pub required_interface_version: u32,
    pub iface: *const RetroVfsInterface,
}

#[repr(C)]
pub struct RetroSubsystemMemoryInfo {
    pub extension: *const c_char,
    pub type_: c_uint,
}

#[repr(C)]
pub struct RetroSubsystemRomInfo {
    pub desc: *const c_char,
    pub valid_extensions: *const c_char,
    pub need_fullpath: bool,
    pub block_extract: bool,
    pub required: bool,
    pub memory: *const RetroSubsystemMemoryInfo,
    pub num_memory: c_uint,
}

#[repr(C)]
pub struct RetroSubsystemInfo {
    pub desc: *const c_char,
    pub ident: *const c_char,
    pub roms: *const RetroSubsystemRomInfo,
    pub num_roms: c_uint,
    pub id: c_uint,
}

#[repr(C)]
pub struct RetroDiskControlCallback {
    pub set_eject_state: RetroSetEjectStateFn,
    pub get_eject_state: RetroGetEjectStateFn,
    pub get_image_index: RetroGetImageIndexFn,
    pub set_image_index: RetroSetImageIndexFn,
    pub get_num_images: RetroGetNumImagesFn,
    pub replace_image_index: RetroReplaceImageIndexFn,
    pub add_image_index: RetroAddImageIndexFn,
}
