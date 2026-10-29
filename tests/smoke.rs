//! Loads the built core as a shared library and drives it like a minimal libretro frontend.
//!
//! The synthetic NES test always runs. To also test real content, set:
//! - `JGENESIS_TEST_ROMS`: paths separated by `;`
//! - `JGENESIS_TEST_SYSTEM_DIR`: directory containing BIOS files (e.g. the `RetroArch` system dir)
//! - `JGENESIS_TEST_DUMP_DIR` (optional): write the last frame of each ROM there as a PPM image
//! - `JGENESIS_TEST_FRAMES` (optional): frames to run each ROM for (default 600)
//! - `JGENESIS_TEST_PRESS_START` (optional): tap Start on port 1 every few seconds, to get past
//!   title screens and "push Run" prompts (e.g. the PC Engine System Card)
//! - `JGENESIS_TEST_SCD_MODE_1` (optional): `<cartridge>;<disc>` to load as Sega CD Mode 1

use libloading::{Library, Symbol};
use std::ffi::{CStr, CString, c_char, c_int, c_uint, c_void};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::{env, fs, ptr, slice};

#[repr(C)]
struct GameInfo {
    path: *const c_char,
    data: *const c_void,
    size: usize,
    meta: *const c_char,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
struct AvInfo {
    base_width: c_uint,
    base_height: c_uint,
    max_width: c_uint,
    max_height: c_uint,
    aspect_ratio: f32,
    fps: f64,
    sample_rate: f64,
}

#[repr(C)]
struct CoreOptionV2Definition {
    key: *const c_char,
    desc: *const c_char,
    desc_categorized: *const c_char,
    info: *const c_char,
    info_categorized: *const c_char,
    category_key: *const c_char,
    values: [[*const c_char; 2]; 128],
    default_value: *const c_char,
}

#[repr(C)]
struct CoreOptionsV2 {
    categories: *const [*const c_char; 3],
    definitions: *const CoreOptionV2Definition,
}

#[repr(C)]
struct SubsystemInfo {
    desc: *const c_char,
    ident: *const c_char,
    roms: *const c_void,
    num_roms: c_uint,
    id: c_uint,
}

/// `retro_vfs_interface` v1-v3, implemented below on top of `std::fs`
#[repr(C)]
struct VfsInterface {
    get_path: Option<unsafe extern "C" fn()>,
    open: unsafe extern "C" fn(*const c_char, c_uint, c_uint) -> *mut c_void,
    close: unsafe extern "C" fn(*mut c_void) -> c_int,
    size: unsafe extern "C" fn(*mut c_void) -> i64,
    tell: unsafe extern "C" fn(*mut c_void) -> i64,
    seek: unsafe extern "C" fn(*mut c_void, i64, c_int) -> i64,
    read: unsafe extern "C" fn(*mut c_void, *mut c_void, u64) -> i64,
    write: unsafe extern "C" fn(*mut c_void, *const c_void, u64) -> i64,
    flush: unsafe extern "C" fn(*mut c_void) -> c_int,
    remove: unsafe extern "C" fn(*const c_char) -> c_int,
    rename: unsafe extern "C" fn(*const c_char, *const c_char) -> c_int,
    truncate: Option<unsafe extern "C" fn()>,
    stat: unsafe extern "C" fn(*const c_char, *mut i32) -> c_int,
    mkdir: unsafe extern "C" fn(*const c_char) -> c_int,
    dir_functions: [Option<unsafe extern "C" fn()>; 5],
}

#[repr(C)]
struct VfsInterfaceInfo {
    required_interface_version: u32,
    iface: *const VfsInterface,
}

static TEST_LOCK: Mutex<()> = Mutex::new(());

static FRAMES: AtomicUsize = AtomicUsize::new(0);
static FRAME_WIDTH: AtomicU32 = AtomicU32::new(0);
static FRAME_HEIGHT: AtomicU32 = AtomicU32::new(0);
static NON_BLACK_PIXELS: AtomicUsize = AtomicUsize::new(0);
static AUDIO_FRAMES: AtomicUsize = AtomicUsize::new(0);
static REGISTERED_OPTIONS: AtomicUsize = AtomicUsize::new(0);
static DUPED_FRAMES: AtomicUsize = AtomicUsize::new(0);
static SERIALIZATION_QUIRKS: AtomicU64 = AtomicU64::new(0);
static SUPPORTS_NO_GAME: AtomicBool = AtomicBool::new(false);
static REGISTERED_SUBSYSTEMS: AtomicUsize = AtomicUsize::new(0);
/// `retro_av_enable_flags` returned by `GET_AUDIO_VIDEO_ENABLE`
static AV_ENABLE: AtomicI32 = AtomicI32::new(0b11);
static DIRS: Mutex<Option<(CString, CString)>> = Mutex::new(None);
static LAST_MESSAGE: Mutex<String> = Mutex::new(String::new());
static LAST_FRAME: Mutex<Vec<u32>> = Mutex::new(Vec::new());

unsafe extern "C" fn environment(cmd: c_uint, data: *mut c_void) -> bool {
    match cmd {
        // GET_SYSTEM_DIRECTORY, GET_SAVE_DIRECTORY
        9 | 31 => {
            let dirs = DIRS.lock().unwrap();
            let (system, save) = dirs.as_ref().unwrap();
            let dir = if cmd == 9 { system } else { save };
            unsafe { *data.cast::<*const c_char>() = dir.as_ptr() };
            true
        }
        // SET_PIXEL_FORMAT
        10 => unsafe { *data.cast::<i32>() == 1 },
        // GET_CORE_OPTIONS_VERSION
        52 => {
            unsafe { *data.cast::<c_uint>() = 2 };
            true
        }
        // SET_CORE_OPTIONS_V2: walk the definitions to make sure they're well-formed
        67 => {
            let options = unsafe { &*data.cast::<CoreOptionsV2>() };
            let mut count = 0;
            loop {
                let definition = unsafe { &*options.definitions.add(count) };
                if definition.key.is_null() {
                    break;
                }
                let default = unsafe { CStr::from_ptr(definition.default_value) };
                let has_default = definition
                    .values
                    .iter()
                    .take_while(|value| !value[0].is_null())
                    .any(|value| unsafe { CStr::from_ptr(value[0]) } == default);
                assert!(has_default, "option {count} default is not one of its values");
                assert!(!definition.category_key.is_null());
                count += 1;
            }
            REGISTERED_OPTIONS.store(count, Ordering::SeqCst);
            true
        }
        // GET_CAN_DUPE
        3 => {
            unsafe { *data.cast::<bool>() = CAN_DUPE.load(Ordering::SeqCst) };
            true
        }
        // GET_INPUT_MAX_USERS
        61 => match u32::try_from(MAX_USERS.load(Ordering::SeqCst)) {
            Ok(users) => {
                unsafe { *data.cast::<c_uint>() = users };
                true
            }
            Err(_) => false,
        },
        // GET_TARGET_SAMPLE_RATE
        0x10051 => match TARGET_SAMPLE_RATE.load(Ordering::SeqCst) {
            0 => false,
            rate => {
                unsafe { *data.cast::<c_uint>() = rate };
                true
            }
        },
        // GET_AUDIO_SAMPLE_BATCH_FLOAT
        0x10055 => {
            if !FLOAT_AUDIO.load(Ordering::SeqCst) {
                return false;
            }
            let callback = unsafe { &mut *data.cast::<AudioSampleFloatCallback>() };
            callback.batch = Some(audio_sample_batch_float);
            true
        }
        // GET_MEMORY_STATUS
        0x10056 => match FREE_MEMORY.load(Ordering::SeqCst) {
            u64::MAX => false,
            free => {
                let status = unsafe { &mut *data.cast::<[u64; 2]>() };
                *status = [free, free];
                true
            }
        },
        // SET_SUPPORT_NO_GAME
        18 => {
            SUPPORTS_NO_GAME.store(unsafe { *data.cast::<bool>() }, Ordering::SeqCst);
            true
        }
        // SET_SUBSYSTEM_INFO: count entries up to the zeroed terminator
        34 => {
            let subsystems = data.cast::<SubsystemInfo>();
            let mut count = 0;
            loop {
                let subsystem = unsafe { &*subsystems.add(count) };
                if subsystem.ident.is_null() {
                    break;
                }
                assert!(!subsystem.desc.is_null() && !subsystem.roms.is_null());
                assert!(subsystem.num_roms > 0);
                count += 1;
            }
            REGISTERED_SUBSYSTEMS.store(count, Ordering::SeqCst);
            true
        }
        // SET_SERIALIZATION_QUIRKS: record them and acknowledge variable-size states
        44 => {
            let quirks = unsafe { &mut *data.cast::<u64>() };
            SERIALIZATION_QUIRKS.store(*quirks, Ordering::SeqCst);
            *quirks |= 1 << 3;
            true
        }
        // GET_VFS_INTERFACE
        0x1002D => {
            let info = unsafe { &mut *data.cast::<VfsInterfaceInfo>() };
            if !USE_VFS.load(Ordering::SeqCst) || info.required_interface_version > 3 {
                return false;
            }
            info.required_interface_version = 3;
            info.iface = &raw const TEST_VFS;
            true
        }
        // GET_AUDIO_VIDEO_ENABLE
        0x1002F => {
            unsafe { *data.cast::<i32>() = AV_ENABLE.load(Ordering::SeqCst) };
            true
        }
        // SET_MESSAGE
        6 => {
            let msg = unsafe { CStr::from_ptr(*data.cast::<*const c_char>()) };
            *LAST_MESSAGE.lock().unwrap() = msg.to_string_lossy().into_owned();
            true
        }
        // SET_GEOMETRY, SET_SYSTEM_AV_INFO, SET_INPUT_DESCRIPTORS, SET_CONTROLLER_INFO,
        // SET_CONTENT_INFO_OVERRIDE, SET_DISK_CONTROL_INTERFACE
        37 | 32 | 11 | 35 | 65 | 13 => true,
        _ => false,
    }
}

unsafe extern "C" fn video_refresh(
    data: *const c_void,
    width: c_uint,
    height: c_uint,
    pitch: usize,
) {
    FRAMES.fetch_add(1, Ordering::SeqCst);
    if data.is_null() {
        DUPED_FRAMES.fetch_add(1, Ordering::SeqCst);
        return;
    }

    assert_eq!(pitch, width as usize * 4);
    FRAME_WIDTH.store(width, Ordering::SeqCst);
    FRAME_HEIGHT.store(height, Ordering::SeqCst);

    let pixels = unsafe { slice::from_raw_parts(data.cast::<u32>(), (width * height) as usize) };
    let non_black = pixels.iter().filter(|&&pixel| pixel & 0x00FF_FFFF != 0).count();
    NON_BLACK_PIXELS.store(non_black, Ordering::SeqCst);

    let mut last_frame = LAST_FRAME.lock().unwrap();
    last_frame.clear();
    last_frame.extend_from_slice(pixels);
}

unsafe extern "C" fn audio_sample_batch(_data: *const i16, frames: usize) -> usize {
    AUDIO_FRAMES.fetch_add(frames, Ordering::SeqCst);
    frames
}

/// Optional frontend capabilities, configured per test. Defaults match `FrontendCaps::default()`.
static CAN_DUPE: AtomicBool = AtomicBool::new(true);
/// Negative means `GET_INPUT_MAX_USERS` is unsupported
static MAX_USERS: AtomicI32 = AtomicI32::new(-1);
/// Zero means `GET_TARGET_SAMPLE_RATE` is unsupported
static TARGET_SAMPLE_RATE: AtomicU32 = AtomicU32::new(0);
static FLOAT_AUDIO: AtomicBool = AtomicBool::new(false);
/// `u64::MAX` means `GET_MEMORY_STATUS` is unsupported
static FREE_MEMORY: AtomicU64 = AtomicU64::new(u64::MAX);

static FLOAT_AUDIO_FRAMES: AtomicUsize = AtomicUsize::new(0);
/// Bitmask of ports the core has read input from
static POLLED_PORTS: AtomicU32 = AtomicU32::new(0);

#[repr(C)]
struct AudioSampleFloatCallback {
    batch: Option<unsafe extern "C" fn(*const f32, usize) -> usize>,
}

unsafe extern "C" fn audio_sample_batch_float(data: *const f32, frames: usize) -> usize {
    let samples = unsafe { slice::from_raw_parts(data, frames * 2) };
    assert!(
        samples.iter().all(|sample| (-1.0..=1.0).contains(sample)),
        "float sample out of range"
    );
    FLOAT_AUDIO_FRAMES.fetch_add(frames, Ordering::SeqCst);
    frames
}

/// Frontend capabilities for one test
struct FrontendCaps {
    can_dupe: bool,
    max_users: Option<u32>,
    target_sample_rate: Option<u32>,
    float_audio: bool,
    free_memory: Option<u64>,
}

impl Default for FrontendCaps {
    fn default() -> Self {
        Self {
            can_dupe: true,
            max_users: None,
            target_sample_rate: None,
            float_audio: false,
            free_memory: None,
        }
    }
}

impl FrontendCaps {
    /// Returns a guard that restores the defaults, even if the test panics
    fn apply(&self) -> FrontendCapsGuard {
        self.store();
        FLOAT_AUDIO_FRAMES.store(0, Ordering::SeqCst);
        POLLED_PORTS.store(0, Ordering::SeqCst);
        FrontendCapsGuard
    }

    fn store(&self) {
        CAN_DUPE.store(self.can_dupe, Ordering::SeqCst);
        MAX_USERS.store(self.max_users.map_or(-1, |users| users as i32), Ordering::SeqCst);
        TARGET_SAMPLE_RATE.store(self.target_sample_rate.unwrap_or(0), Ordering::SeqCst);
        FLOAT_AUDIO.store(self.float_audio, Ordering::SeqCst);
        FREE_MEMORY.store(self.free_memory.unwrap_or(u64::MAX), Ordering::SeqCst);
    }
}

/// Separate from `FrontendCaps` so that `..FrontendCaps::default()` temporaries don't reset
/// anything when they're dropped
struct FrontendCapsGuard;

impl Drop for FrontendCapsGuard {
    fn drop(&mut self) {
        FrontendCaps::default().store();
    }
}

unsafe extern "C" fn input_poll() {}

/// Offer the test VFS to the core
static USE_VFS: AtomicBool = AtomicBool::new(false);
static VFS_READ_OPENS: AtomicUsize = AtomicUsize::new(0);
static VFS_WRITE_OPENS: AtomicUsize = AtomicUsize::new(0);

static TEST_VFS: VfsInterface = VfsInterface {
    get_path: None,
    open: vfs_open,
    close: vfs_close,
    size: vfs_size,
    tell: vfs_tell,
    seek: vfs_seek,
    read: vfs_read,
    write: vfs_write,
    flush: vfs_flush,
    remove: vfs_remove,
    rename: vfs_rename,
    truncate: None,
    stat: vfs_stat,
    mkdir: vfs_mkdir,
    dir_functions: [None; 5],
};

/// Paths under this prefix only exist through the test VFS, like a frontend's `smb://` paths
const VFS_ONLY: &str = "vfsonly://";

/// Paths the test VFS has opened for reading
static VFS_READ_PATHS: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn vfs_path(path: *const c_char) -> PathBuf {
    let path = unsafe { CStr::from_ptr(path) }.to_str().unwrap();
    PathBuf::from(path.strip_prefix(VFS_ONLY).unwrap_or(path))
}

fn vfs_file<'a>(handle: *mut c_void) -> &'a mut fs::File {
    unsafe { &mut *handle.cast::<fs::File>() }
}

unsafe extern "C" fn vfs_open(path: *const c_char, mode: c_uint, _hints: c_uint) -> *mut c_void {
    const WRITE: c_uint = 1 << 1;
    let path = vfs_path(path);
    let file = if mode & WRITE != 0 {
        VFS_WRITE_OPENS.fetch_add(1, Ordering::SeqCst);
        fs::File::create(path)
    } else {
        VFS_READ_OPENS.fetch_add(1, Ordering::SeqCst);
        VFS_READ_PATHS.lock().unwrap().push(path.display().to_string());
        fs::File::open(path)
    };
    file.map_or(ptr::null_mut(), |file| Box::into_raw(Box::new(file)).cast())
}

unsafe extern "C" fn vfs_close(handle: *mut c_void) -> c_int {
    drop(unsafe { Box::from_raw(handle.cast::<fs::File>()) });
    0
}

unsafe extern "C" fn vfs_size(handle: *mut c_void) -> i64 {
    vfs_file(handle).metadata().map_or(-1, |metadata| metadata.len() as i64)
}

unsafe extern "C" fn vfs_tell(handle: *mut c_void) -> i64 {
    vfs_file(handle).stream_position().map_or(-1, |position| position as i64)
}

unsafe extern "C" fn vfs_seek(handle: *mut c_void, offset: i64, whence: c_int) -> i64 {
    let pos = match whence {
        0 => SeekFrom::Start(offset as u64),
        1 => SeekFrom::Current(offset),
        _ => SeekFrom::End(offset),
    };
    vfs_file(handle).seek(pos).map_or(-1, |position| position as i64)
}

unsafe extern "C" fn vfs_read(handle: *mut c_void, buf: *mut c_void, len: u64) -> i64 {
    let buf = unsafe { slice::from_raw_parts_mut(buf.cast::<u8>(), len as usize) };
    vfs_file(handle).read(buf).map_or(-1, |len| len as i64)
}

unsafe extern "C" fn vfs_write(handle: *mut c_void, buf: *const c_void, len: u64) -> i64 {
    let buf = unsafe { slice::from_raw_parts(buf.cast::<u8>(), len as usize) };
    vfs_file(handle).write(buf).map_or(-1, |len| len as i64)
}

unsafe extern "C" fn vfs_flush(handle: *mut c_void) -> c_int {
    vfs_file(handle).flush().map_or(-1, |()| 0)
}

unsafe extern "C" fn vfs_remove(path: *const c_char) -> c_int {
    fs::remove_file(vfs_path(path)).map_or(-1, |()| 0)
}

unsafe extern "C" fn vfs_rename(old: *const c_char, new: *const c_char) -> c_int {
    let (old, new) = (vfs_path(old), vfs_path(new));
    // Like the libretro spec, refuse to overwrite an existing file
    if new.exists() {
        return -1;
    }
    fs::rename(old, new).map_or(-1, |()| 0)
}

unsafe extern "C" fn vfs_stat(path: *const c_char, size: *mut i32) -> c_int {
    const IS_VALID: c_int = 1 << 0;
    const IS_DIRECTORY: c_int = 1 << 1;
    let Ok(metadata) = fs::metadata(vfs_path(path)) else { return 0 };
    if !size.is_null() {
        unsafe { *size = metadata.len() as i32 };
    }
    if metadata.is_dir() { IS_VALID | IS_DIRECTORY } else { IS_VALID }
}

unsafe extern "C" fn vfs_mkdir(path: *const c_char) -> c_int {
    match fs::create_dir(vfs_path(path)) {
        Ok(()) => 0,
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => -2,
        Err(_) => -1,
    }
}

/// Stops offering the test VFS when dropped, even if the test panics
struct VfsGuard;

impl VfsGuard {
    fn enable() -> Self {
        VFS_READ_OPENS.store(0, Ordering::SeqCst);
        VFS_WRITE_OPENS.store(0, Ordering::SeqCst);
        VFS_READ_PATHS.lock().unwrap().clear();
        USE_VFS.store(true, Ordering::SeqCst);
        Self
    }
}

impl Drop for VfsGuard {
    fn drop(&mut self) {
        USE_VFS.store(false, Ordering::SeqCst);
    }
}

static PRESS_START: AtomicBool = AtomicBool::new(false);

unsafe extern "C" fn input_state(port: c_uint, device: c_uint, _index: c_uint, id: c_uint) -> i16 {
    const JOYPAD: c_uint = 1;
    const START: c_uint = 3;

    POLLED_PORTS.fetch_or(1 << port, Ordering::SeqCst);

    // Hold Start for 10 frames out of every 240
    let pressed = PRESS_START.load(Ordering::SeqCst)
        && (port, device, id) == (0, JOYPAD, START)
        && FRAMES.load(Ordering::SeqCst) % 240 >= 230;
    pressed.into()
}

fn core_path() -> PathBuf {
    // Test binaries live in target/<profile>/deps. `cargo test` builds the cdylib there too (it's
    // only copied up to target/<profile> by `cargo build`), so prefer that fresh copy.
    let exe = env::current_exe().unwrap();
    let deps_dir = exe.parent().unwrap();
    let name = format!("{}jgenesis_libretro{}", env::consts::DLL_PREFIX, env::consts::DLL_SUFFIX);
    [deps_dir, deps_dir.parent().unwrap()]
        .into_iter()
        .map(|dir| dir.join(&name))
        .find(|path| path.exists())
        .unwrap_or_else(|| deps_dir.join(&name))
}

struct Core {
    lib: Library,
}

impl Core {
    fn load(system_dir: &Path, save_dir: &Path) -> Self {
        *DIRS.lock().unwrap() = Some((
            CString::new(system_dir.to_str().unwrap()).unwrap(),
            CString::new(save_dir.to_str().unwrap()).unwrap(),
        ));

        let path = core_path();
        let lib = unsafe { Library::new(&path) }
            .unwrap_or_else(|err| panic!("Unable to load core at '{}': {err}", path.display()));
        let core = Self { lib };

        unsafe {
            assert_eq!(core.sym::<unsafe extern "C" fn() -> c_uint>(b"retro_api_version")(), 1);
            core.sym::<unsafe extern "C" fn(unsafe extern "C" fn(c_uint, *mut c_void) -> bool)>(
                b"retro_set_environment",
            )(environment);
            core.sym::<unsafe extern "C" fn(
                unsafe extern "C" fn(*const c_void, c_uint, c_uint, usize),
            )>(b"retro_set_video_refresh")(video_refresh);
            core.sym::<unsafe extern "C" fn(unsafe extern "C" fn(*const i16, usize) -> usize)>(
                b"retro_set_audio_sample_batch",
            )(audio_sample_batch);
            core.sym::<unsafe extern "C" fn(unsafe extern "C" fn())>(b"retro_set_input_poll")(
                input_poll,
            );
            core.sym::<unsafe extern "C" fn(
                unsafe extern "C" fn(c_uint, c_uint, c_uint, c_uint) -> i16,
            )>(b"retro_set_input_state")(input_state);
            core.sym::<unsafe extern "C" fn()>(b"retro_init")();
        }

        core
    }

    unsafe fn sym<T>(&self, name: &[u8]) -> Symbol<'_, T> {
        unsafe { self.lib.get(name) }.unwrap()
    }

    fn load_game(&self, path: &Path, data: Option<&[u8]>) -> bool {
        let path = CString::new(path.to_str().unwrap()).unwrap();
        let info = GameInfo {
            path: path.as_ptr(),
            data: data.map_or(ptr::null(), |data| data.as_ptr().cast()),
            size: data.map_or(0, <[u8]>::len),
            meta: ptr::null(),
        };
        unsafe {
            self.sym::<unsafe extern "C" fn(*const GameInfo) -> bool>(b"retro_load_game")(
                &raw const info,
            )
        }
    }

    fn load_no_game(&self) -> bool {
        unsafe {
            self.sym::<unsafe extern "C" fn(*const GameInfo) -> bool>(b"retro_load_game")(
                ptr::null(),
            )
        }
    }

    fn load_game_special(&self, game_type: c_uint, paths: &[&Path]) -> bool {
        let paths: Vec<_> =
            paths.iter().map(|path| CString::new(path.to_str().unwrap()).unwrap()).collect();
        let infos: Vec<_> = paths
            .iter()
            .map(|path| GameInfo {
                path: path.as_ptr(),
                data: ptr::null(),
                size: 0,
                meta: ptr::null(),
            })
            .collect();
        unsafe {
            self.sym::<unsafe extern "C" fn(c_uint, *const GameInfo, usize) -> bool>(
                b"retro_load_game_special",
            )(game_type, infos.as_ptr(), infos.len())
        }
    }

    fn av_info(&self) -> AvInfo {
        let mut av_info = AvInfo::default();
        unsafe {
            self.sym::<unsafe extern "C" fn(*mut AvInfo)>(b"retro_get_system_av_info")(
                &raw mut av_info,
            );
        }
        av_info
    }

    fn run(&self, frames: usize) {
        let run = unsafe { self.sym::<unsafe extern "C" fn()>(b"retro_run") };
        for _ in 0..frames {
            unsafe { run() };
        }
    }

    fn serialize(&self) -> Vec<u8> {
        unsafe {
            let size = self.sym::<unsafe extern "C" fn() -> usize>(b"retro_serialize_size")();
            assert!(size > 0, "serialize size should be nonzero");
            let mut buffer = vec![0_u8; size];
            let ok = self
                .sym::<unsafe extern "C" fn(*mut c_void, usize) -> bool>(b"retro_serialize")(
                buffer.as_mut_ptr().cast(),
                size,
            );
            assert!(ok, "retro_serialize failed");
            buffer
        }
    }

    fn unserialize(&self, state: &[u8]) -> bool {
        unsafe {
            self.sym::<unsafe extern "C" fn(*const c_void, usize) -> bool>(b"retro_unserialize")(
                state.as_ptr().cast(),
                state.len(),
            )
        }
    }

    fn reset(&self) {
        unsafe { self.sym::<unsafe extern "C" fn()>(b"retro_reset")() };
    }

    fn unload(self) {
        unsafe {
            self.sym::<unsafe extern "C" fn()>(b"retro_unload_game")();
            self.sym::<unsafe extern "C" fn()>(b"retro_deinit")();
        }
    }
}

fn reset_counters() {
    FRAMES.store(0, Ordering::SeqCst);
    DUPED_FRAMES.store(0, Ordering::SeqCst);
    AUDIO_FRAMES.store(0, Ordering::SeqCst);
    NON_BLACK_PIXELS.store(0, Ordering::SeqCst);
}

/// Write the last frame as a binary PPM image.
fn dump_last_frame(path: &Path) {
    let (width, height) = (FRAME_WIDTH.load(Ordering::SeqCst), FRAME_HEIGHT.load(Ordering::SeqCst));
    let mut ppm = format!("P6\n{width} {height}\n255\n").into_bytes();
    for &pixel in LAST_FRAME.lock().unwrap().iter() {
        ppm.extend_from_slice(&[(pixel >> 16) as u8, (pixel >> 8) as u8, pixel as u8]);
    }
    fs::write(path, ppm).unwrap();
}

/// Smallest possible NROM image: the CPU spins in a `JMP` loop with rendering disabled.
fn synthetic_nes_rom() -> Vec<u8> {
    let mut rom = vec![0_u8; 16 + 0x4000 + 0x2000];
    rom[..8].copy_from_slice(b"NES\x1A\x01\x01\x00\x00");

    let prg = &mut rom[16..16 + 0x4000];
    // $C000: JMP $C000
    prg[..3].copy_from_slice(&[0x4C, 0x00, 0xC0]);
    // NMI, RESET, IRQ vectors all point to $C000
    prg[0x3FFA..].copy_from_slice(&[0x00, 0xC0, 0x00, 0xC0, 0x00, 0xC0]);

    rom
}

/// Battery-backed MMC1 ROM that writes $42 to the first byte of PRG RAM, producing a .sav file
fn battery_nes_rom() -> Vec<u8> {
    let mut rom = synthetic_nes_rom();
    // Mapper 1 (MMC1; NROM ignores PRG RAM writes), battery-backed PRG RAM
    rom[6] = 0x12;

    let prg = &mut rom[16..16 + 0x4000];
    // $C000: LDA #$42; STA $6000; $C005: JMP $C005
    prg[..8].copy_from_slice(&[0xA9, 0x42, 0x8D, 0x00, 0x60, 0x4C, 0x05, 0xC0]);

    rom
}

/// 32 KiB MBC1 cartridge with 8 KiB battery-backed RAM that increments the first byte of cartridge
/// RAM once per boot, so the .sav file counts how many times the game started
fn boot_counter_gb_rom() -> Vec<u8> {
    let mut rom = vec![0_u8; 0x8000];
    // Entry point: NOP; JP $0150
    rom[0x100..0x104].copy_from_slice(&[0x00, 0xC3, 0x50, 0x01]);
    // MBC1+RAM+BATTERY, 32 KiB ROM, 8 KiB RAM
    rom[0x147..0x14A].copy_from_slice(&[0x03, 0x00, 0x02]);
    rom[0x150..0x15F].copy_from_slice(&[
        0x3E, 0x0A, // LD A, $0A
        0xEA, 0x00, 0x00, // LD ($0000), A (enable cartridge RAM)
        0xFA, 0x00, 0xA0, // LD A, ($A000)
        0x3C, // INC A
        0xEA, 0x00, 0xA0, // LD ($A000), A
        0x18, 0xFE, // JR -2
        0x00,
    ]);
    rom
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = env::temp_dir().join(format!("jgenesis-libretro-test-{name}-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn synthetic_nes_rom_runs_and_round_trips_state() {
    let _lock = TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    reset_counters();

    let dir = temp_dir("nes");
    let rom_path = dir.join("loop.nes");
    let rom = synthetic_nes_rom();
    fs::write(&rom_path, &rom).unwrap();

    let core = Core::load(&dir, &dir);
    assert!(REGISTERED_OPTIONS.load(Ordering::SeqCst) > 0, "core options were not registered");
    assert!(SUPPORTS_NO_GAME.load(Ordering::SeqCst), "no-game support was not declared");
    assert_eq!(REGISTERED_SUBSYSTEMS.load(Ordering::SeqCst), 1);
    assert_eq!(SERIALIZATION_QUIRKS.load(Ordering::SeqCst), 1 << 2, "expected CORE_VARIABLE_SIZE");
    assert!(core.load_game(&rom_path, Some(&rom)), "retro_load_game failed");

    let av_info = core.av_info();
    assert_eq!((av_info.base_width, av_info.base_height), (256, 224), "{av_info:?}");
    assert!((av_info.fps - 60.1).abs() < 0.1, "{av_info:?}");
    assert_eq!(av_info.sample_rate, 48000.0);

    core.run(120);
    assert_eq!(FRAMES.load(Ordering::SeqCst), 120);
    assert_eq!(FRAME_WIDTH.load(Ordering::SeqCst), 256);
    assert_eq!(FRAME_HEIGHT.load(Ordering::SeqCst), 224);

    // ~800 stereo frames per video frame at 48 kHz / 60 Hz
    let audio_frames = AUDIO_FRAMES.load(Ordering::SeqCst);
    assert!(
        (90_000..102_000).contains(&audio_frames),
        "unexpected audio frame count {audio_frames}"
    );

    // Frames the frontend will discard (e.g. run-ahead) produce no video or audio
    let audio_frames = AUDIO_FRAMES.load(Ordering::SeqCst);
    AV_ENABLE.store(0, Ordering::SeqCst);
    core.run(10);
    AV_ENABLE.store(0b11, Ordering::SeqCst);
    assert_eq!(DUPED_FRAMES.load(Ordering::SeqCst), 10);
    assert_eq!(AUDIO_FRAMES.load(Ordering::SeqCst), audio_frames);

    let state = core.serialize();
    core.run(30);
    assert!(core.unserialize(&state), "retro_unserialize failed");
    assert!(!core.unserialize(&state[..state.len() / 2]), "truncated state should be rejected");
    assert!(!core.unserialize(b"not a save state"), "garbage state should be rejected");
    core.reset();
    core.run(30);

    core.unload();
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn real_roms_render_frames() {
    let _lock = TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);

    let Ok(roms) = env::var("JGENESIS_TEST_ROMS") else {
        eprintln!("JGENESIS_TEST_ROMS not set; skipping");
        return;
    };
    let system_dir =
        env::var("JGENESIS_TEST_SYSTEM_DIR").map_or_else(|_| temp_dir("system"), PathBuf::from);
    let save_dir = temp_dir("saves");
    let frames = env::var("JGENESIS_TEST_FRAMES").map_or(600, |frames| frames.parse().unwrap());
    PRESS_START.store(env::var_os("JGENESIS_TEST_PRESS_START").is_some(), Ordering::SeqCst);

    let mut failures = vec![];
    for rom in roms.split(';').filter(|rom| !rom.is_empty()) {
        reset_counters();
        let path = Path::new(rom);
        let data = fs::read(path).unwrap_or_else(|err| panic!("{rom}: {err}"));
        let is_disc =
            matches!(path.extension().and_then(|ext| ext.to_str()), Some("cue" | "chd" | "m3u"));

        let core = Core::load(&system_dir, &save_dir);
        if !core.load_game(path, (!is_disc).then_some(&data[..])) {
            failures.push(format!("{rom}: retro_load_game failed"));
            core.unload();
            continue;
        }

        core.run(frames);
        let av_info = core.av_info();
        let (width, height) =
            (FRAME_WIDTH.load(Ordering::SeqCst), FRAME_HEIGHT.load(Ordering::SeqCst));
        let non_black = NON_BLACK_PIXELS.load(Ordering::SeqCst);
        eprintln!(
            "{rom}: {width}x{height} @ {:.3} fps, aspect {:.3}, {non_black} non-black pixels, {} audio frames",
            av_info.fps,
            av_info.aspect_ratio,
            AUDIO_FRAMES.load(Ordering::SeqCst)
        );
        if non_black == 0 {
            failures.push(format!("{rom}: last frame was entirely black"));
        }
        if let Ok(dump_dir) = env::var("JGENESIS_TEST_DUMP_DIR") {
            let name = path.file_stem().unwrap().to_string_lossy();
            dump_last_frame(&Path::new(&dump_dir).join(format!("{name}.ppm")));
        }

        let state = core.serialize();
        core.run(10);
        if !core.unserialize(&state) {
            failures.push(format!("{rom}: unserialize failed"));
        }

        core.unload();
    }

    assert!(failures.is_empty(), "failures:\n{}", failures.join("\n"));
}

#[test]
fn no_game_boots_sega_cd_bios() {
    let _lock = TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);

    let Ok(system_dir) = env::var("JGENESIS_TEST_SYSTEM_DIR") else {
        eprintln!("JGENESIS_TEST_SYSTEM_DIR not set; skipping");
        return;
    };
    reset_counters();
    let save_dir = temp_dir("no-game");

    let core = Core::load(Path::new(&system_dir), &save_dir);
    assert!(core.load_no_game(), "retro_load_game(NULL) failed");

    // The BIOS logo animation takes a few seconds to appear
    core.run(600);
    assert!(NON_BLACK_PIXELS.load(Ordering::SeqCst) > 0, "Sega CD BIOS screen was entirely black");

    core.unload();
    fs::remove_dir_all(&save_dir).ok();
}

#[test]
fn sega_cd_mode_1_subsystem() {
    let _lock = TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);

    let (Ok(paths), Ok(system_dir)) =
        (env::var("JGENESIS_TEST_SCD_MODE_1"), env::var("JGENESIS_TEST_SYSTEM_DIR"))
    else {
        eprintln!("JGENESIS_TEST_SCD_MODE_1 or JGENESIS_TEST_SYSTEM_DIR not set; skipping");
        return;
    };
    let (cartridge, disc) = paths.split_once(';').expect("expected <cartridge>;<disc>");
    reset_counters();
    let save_dir = temp_dir("scd-mode-1");

    let core = Core::load(Path::new(&system_dir), &save_dir);
    assert!(
        core.load_game_special(1, &[Path::new(cartridge), Path::new(disc)]),
        "retro_load_game_special failed"
    );

    core.run(600);
    assert!(NON_BLACK_PIXELS.load(Ordering::SeqCst) > 0, "last frame was entirely black");

    core.unload();
    fs::remove_dir_all(&save_dir).ok();
}

#[test]
fn vfs_is_used_for_content_and_saves() {
    let _lock = TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    reset_counters();
    let _vfs = VfsGuard::enable();

    let dir = temp_dir("vfs");
    let rom_path = dir.join("battery.nes");
    fs::write(&rom_path, battery_nes_rom()).unwrap();
    // Doesn't exist yet, so the core has to create it through the VFS
    let save_dir = dir.join("saves/nested");

    let core = Core::load(&dir, &save_dir);
    // Without data, the core reads the ROM itself
    assert!(core.load_game(&rom_path, None), "retro_load_game failed");
    assert!(VFS_READ_OPENS.load(Ordering::SeqCst) > 0, "ROM was not read through the VFS");

    core.run(10);
    core.unload();
    assert!(VFS_WRITE_OPENS.load(Ordering::SeqCst) > 0, "save was not written through the VFS");
    let save = fs::read(save_dir.join("battery.sav")).expect("save file was not written");
    assert_eq!(save[0], 0x42);
    assert!(!save_dir.join("battery.tmp").exists());

    // Saving again has to replace the existing file
    let core = Core::load(&dir, &save_dir);
    assert!(core.load_game(&rom_path, None), "retro_load_game failed with an existing save");
    core.run(10);
    core.unload();
    assert_eq!(fs::read(save_dir.join("battery.sav")).unwrap()[0], 0x42);

    fs::remove_dir_all(&dir).ok();
}

#[test]
fn vfs_only_m3u_and_cue_paths() {
    let _lock = TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    reset_counters();
    let _vfs = VfsGuard::enable();

    let dir = temp_dir("vfs-only");
    fs::write(dir.join("game.m3u"), "disc.cue\n").unwrap();
    fs::write(dir.join("disc.cue"), "FILE \"disc.bin\" BINARY\n  TRACK 01 MODE1/2352\n").unwrap();
    let m3u = PathBuf::from(format!("{VFS_ONLY}{}", dir.join("game.m3u").display()));

    // The playlist is only reachable through the VFS, so reading it proves the core used the VFS.
    // The disc it lists then fails to open, because upstream cdrom reads CUE/BIN with std::fs.
    let core = Core::load(&dir, &dir);
    assert!(!core.load_game(&m3u, None), "CUE through a VFS-only path should not load yet");
    let paths = VFS_READ_PATHS.lock().unwrap().clone();
    assert!(paths.iter().any(|path| path.ends_with("game.m3u")), "M3U not read via VFS: {paths:?}");

    let message = LAST_MESSAGE.lock().unwrap().clone();
    assert!(message.contains("can't be loaded from this location"), "unexpected: {message}");
    assert!(message.contains("disc.cue"), "unexpected: {message}");

    core.unload();
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn vfs_only_firmware() {
    let _lock = TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);

    let Ok(system_dir) = env::var("JGENESIS_TEST_SYSTEM_DIR") else {
        eprintln!("JGENESIS_TEST_SYSTEM_DIR not set; skipping");
        return;
    };
    reset_counters();
    let _vfs = VfsGuard::enable();
    let save_dir = temp_dir("vfs-firmware");

    // Firmware is only reachable through the VFS
    let core = Core::load(Path::new(&format!("{VFS_ONLY}{system_dir}")), &save_dir);
    assert!(core.load_no_game(), "retro_load_game(NULL) failed");
    let paths = VFS_READ_PATHS.lock().unwrap().clone();
    assert!(
        paths.iter().any(|path| path.ends_with("bios_CD_U.bin")),
        "BIOS not read via VFS: {paths:?}"
    );

    core.run(600);
    assert!(NON_BLACK_PIXELS.load(Ordering::SeqCst) > 0, "Sega CD BIOS screen was entirely black");

    core.unload();
    fs::remove_dir_all(&save_dir).ok();
}

#[test]
fn frontend_without_dupe_gets_repeated_frames() {
    let _lock = TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    reset_counters();
    let _caps = FrontendCaps { can_dupe: false, ..FrontendCaps::default() }.apply();

    let dir = temp_dir("no-dupe");
    let rom_path = dir.join("loop.nes");
    let rom = synthetic_nes_rom();
    fs::write(&rom_path, &rom).unwrap();

    let core = Core::load(&dir, &dir);
    assert!(core.load_game(&rom_path, Some(&rom)), "retro_load_game failed");
    core.run(5);

    // Hidden frames must resend the last frame instead of passing NULL
    AV_ENABLE.store(0, Ordering::SeqCst);
    core.run(10);
    AV_ENABLE.store(0b11, Ordering::SeqCst);
    assert_eq!(FRAMES.load(Ordering::SeqCst), 15);
    assert_eq!(DUPED_FRAMES.load(Ordering::SeqCst), 0);

    core.unload();
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn float_audio_at_target_sample_rate() {
    let _lock = TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    reset_counters();
    let _caps = FrontendCaps {
        target_sample_rate: Some(44_100),
        float_audio: true,
        ..FrontendCaps::default()
    }
    .apply();

    let dir = temp_dir("float-audio");
    let rom_path = dir.join("loop.nes");
    let rom = synthetic_nes_rom();
    fs::write(&rom_path, &rom).unwrap();

    let core = Core::load(&dir, &dir);
    assert!(core.load_game(&rom_path, Some(&rom)), "retro_load_game failed");
    assert_eq!(core.av_info().sample_rate, 44_100.0);

    core.run(120);
    // ~734 stereo frames per video frame at 44.1 kHz / 60.1 Hz, all as float
    let float_frames = FLOAT_AUDIO_FRAMES.load(Ordering::SeqCst);
    assert!(
        (83_000..93_000).contains(&float_frames),
        "unexpected float frame count {float_frames}"
    );
    assert_eq!(AUDIO_FRAMES.load(Ordering::SeqCst), 0, "int16 and float audio must not be mixed");

    core.unload();
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn input_max_users_limits_polling() {
    let _lock = TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    reset_counters();
    let _caps = FrontendCaps { max_users: Some(1), ..FrontendCaps::default() }.apply();

    let dir = temp_dir("max-users");
    let rom_path = dir.join("loop.nes");
    let rom = synthetic_nes_rom();
    fs::write(&rom_path, &rom).unwrap();

    let core = Core::load(&dir, &dir);
    assert!(core.load_game(&rom_path, Some(&rom)), "retro_load_game failed");
    core.run(10);
    // The NES has 2 ports, but the frontend only has a device for the first
    assert_eq!(POLLED_PORTS.load(Ordering::SeqCst), 0b1);

    core.unload();
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn reset_power_cycles_systems_without_reset_button() {
    let _lock = TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    reset_counters();

    let dir = temp_dir("gb-reset");
    let rom_path = dir.join("counter.gb");
    let rom = boot_counter_gb_rom();
    fs::write(&rom_path, &rom).unwrap();
    fs::write(dir.join("counter.sav"), vec![0_u8; 0x2000]).unwrap();

    // The Game Boy has no reset button, so retro_reset has to restart the game
    let core = Core::load(&dir, &dir);
    assert!(core.load_game(&rom_path, Some(&rom)), "retro_load_game failed");
    core.run(10);
    core.reset();
    core.run(10);
    core.unload();
    assert_eq!(fs::read(dir.join("counter.sav")).unwrap()[0], 2, "game did not restart");

    fs::remove_dir_all(&dir).ok();
}

#[test]
fn state_from_another_sample_rate_plays_at_this_one() {
    let _lock = TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);

    let dir = temp_dir("state-rate");
    let rom_path = dir.join("loop.nes");
    let rom = synthetic_nes_rom();
    fs::write(&rom_path, &rom).unwrap();

    // Save at 48 kHz...
    let core = Core::load(&dir, &dir);
    assert!(core.load_game(&rom_path, Some(&rom)), "retro_load_game failed");
    core.run(10);
    let state = core.serialize();
    core.unload();

    // ...then load on a frontend running at 32 kHz
    reset_counters();
    let _caps =
        FrontendCaps { target_sample_rate: Some(32_000), ..FrontendCaps::default() }.apply();
    let core = Core::load(&dir, &dir);
    assert!(core.load_game(&rom_path, Some(&rom)), "retro_load_game failed");
    assert!(core.unserialize(&state), "retro_unserialize failed");
    AUDIO_FRAMES.store(0, Ordering::SeqCst);
    core.run(120);

    // ~533 stereo frames per video frame at 32 kHz / 60.1 Hz, not ~800 as at 48 kHz
    let audio_frames = AUDIO_FRAMES.load(Ordering::SeqCst);
    assert!(
        (60_000..68_000).contains(&audio_frames),
        "unexpected audio frame count {audio_frames}"
    );

    core.unload();
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn chd_too_large_for_free_memory_is_refused() {
    let _lock = TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    reset_counters();
    let _vfs = VfsGuard::enable();
    let _caps = FrontendCaps { free_memory: Some(1024 * 1024), ..FrontendCaps::default() }.apply();

    // With a VFS, CHDs are read into memory; this one is bigger than the frontend has free
    let dir = temp_dir("chd-memory");
    let chd_path = dir.join("big.chd");
    fs::write(&chd_path, vec![0_u8; 2 * 1024 * 1024]).unwrap();

    let core = Core::load(&dir, &dir);
    assert!(!core.load_game(&chd_path, None), "oversized CHD should not load");
    let message = LAST_MESSAGE.lock().unwrap().clone();
    assert!(message.contains("Not enough free memory"), "unexpected: {message}");

    core.unload();
    fs::remove_dir_all(&dir).ok();
}

#[test]
fn missing_chd_is_not_opened_through_vfs() {
    let _lock = TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    reset_counters();
    let _vfs = VfsGuard::enable();
    let _caps = FrontendCaps { free_memory: Some(1024 * 1024), ..FrontendCaps::default() }.apply();

    let dir = temp_dir("chd-missing");
    let core = Core::load(&dir, &dir);
    assert!(!core.load_game(&dir.join("missing.chd"), None), "missing CHD should not load");

    // The core checks that the file exists instead of making the frontend log an open failure
    let paths = VFS_READ_PATHS.lock().unwrap().clone();
    assert!(!paths.iter().any(|path| path.ends_with("missing.chd")), "opened: {paths:?}");
    let message = LAST_MESSAGE.lock().unwrap().clone();
    assert!(message.contains("not found"), "unexpected: {message}");

    core.unload();
    fs::remove_dir_all(&dir).ok();
}
