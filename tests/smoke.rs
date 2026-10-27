//! Loads the built core as a shared library and drives it like a minimal libretro frontend.
//!
//! The synthetic NES test always runs. To also test real content, set:
//! - `JGENESIS_TEST_ROMS`: paths separated by `;`
//! - `JGENESIS_TEST_SYSTEM_DIR`: directory containing BIOS files (e.g. the `RetroArch` system dir)
//! - `JGENESIS_TEST_DUMP_DIR` (optional): write the last frame of each ROM there as a PPM image

use libloading::{Library, Symbol};
use std::ffi::{CStr, CString, c_char, c_uint, c_void};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
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

static TEST_LOCK: Mutex<()> = Mutex::new(());

static FRAMES: AtomicUsize = AtomicUsize::new(0);
static FRAME_WIDTH: AtomicU32 = AtomicU32::new(0);
static FRAME_HEIGHT: AtomicU32 = AtomicU32::new(0);
static NON_BLACK_PIXELS: AtomicUsize = AtomicUsize::new(0);
static AUDIO_FRAMES: AtomicUsize = AtomicUsize::new(0);
static REGISTERED_OPTIONS: AtomicUsize = AtomicUsize::new(0);
static DIRS: Mutex<Option<(CString, CString)>> = Mutex::new(None);
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
        // SET_GEOMETRY, SET_SYSTEM_AV_INFO, SET_INPUT_DESCRIPTORS, SET_CONTROLLER_INFO,
        // SET_CONTENT_INFO_OVERRIDE, SET_DISK_CONTROL_INTERFACE, SET_MESSAGE
        37 | 32 | 11 | 35 | 65 | 13 | 6 => true,
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

unsafe extern "C" fn input_poll() {}

unsafe extern "C" fn input_state(
    _port: c_uint,
    _device: c_uint,
    _index: c_uint,
    _id: c_uint,
) -> i16 {
    0
}

fn core_path() -> PathBuf {
    // Test binaries live in target/<profile>/deps; the cdylib is in target/<profile>
    let exe = env::current_exe().unwrap();
    let dir = exe.parent().unwrap().parent().unwrap();
    let name = format!("{}jgenesis_libretro{}", env::consts::DLL_PREFIX, env::consts::DLL_SUFFIX);
    dir.join(name)
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

        core.run(600);
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
