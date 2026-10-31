//! libretro core for [jgenesis](https://github.com/jsgroth/jgenesis).

mod detect;
mod env;
mod ffi;
mod frontend;
mod load;
mod options;
mod session;
mod systems;
mod vfs;

use detect::Console;
use ffi::{RetroGameInfo, RetroSystemAvInfo, RetroSystemContentInfoOverride, RetroSystemInfo};
use load::LoadedGame;
use options::Options;
use std::any::Any;
use std::ffi::{CStr, c_char, c_uint, c_void};
use std::panic::{self, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::{ptr, slice};

const LIBRARY_NAME: &CStr = c"jgenesis";
const LIBRARY_VERSION: &CStr =
    match CStr::from_bytes_with_nul(concat!(env!("CARGO_PKG_VERSION"), "\0").as_bytes()) {
        Ok(version) => version,
        Err(_) => panic!("package version contains a NUL byte"),
    };

struct Game {
    loaded: LoadedGame,
    disc_index: usize,
    disc_ejected: bool,
}

struct CoreState {
    game: Option<Game>,
}

// SAFETY: libretro requires frontends to call the core's API from one thread at a time, so the
// non-Send emulator state is never accessed concurrently
unsafe impl Send for CoreState {}

static CORE: Mutex<CoreState> = Mutex::new(CoreState { game: None });

/// Cached separately from CORE so that it can be read even if the frontend calls
/// `retro_get_system_av_info` from inside another callback.
static AV_INFO: Mutex<Option<RetroSystemAvInfo>> = Mutex::new(None);

fn core() -> MutexGuard<'static, CoreState> {
    CORE.lock().unwrap_or_else(PoisonError::into_inner)
}

fn set_av_info(av_info: Option<RetroSystemAvInfo>) {
    *AV_INFO.lock().unwrap_or_else(PoisonError::into_inner) = av_info;
}

fn panic_message(payload: &(dyn Any + Send)) -> &str {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("unknown panic")
}

/// Run `f`, catching any panic so that it does not unwind into the frontend.
fn guard<T>(name: &str, default: T, f: impl FnOnce() -> T) -> T {
    panic::catch_unwind(AssertUnwindSafe(f)).unwrap_or_else(|payload| {
        log::error!("Panic in {name}: {}", panic_message(&*payload));
        default
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn retro_api_version() -> c_uint {
    ffi::RETRO_API_VERSION
}

/// # Safety
///
/// `info` must be a valid pointer to a `retro_system_info`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn retro_get_system_info(info: *mut RetroSystemInfo) {
    if info.is_null() {
        return;
    }

    // SAFETY: Checked non-null above; frontend guarantees validity
    unsafe {
        *info = RetroSystemInfo {
            library_name: LIBRARY_NAME.as_ptr(),
            library_version: LIBRARY_VERSION.as_ptr(),
            valid_extensions: detect::VALID_EXTENSIONS.as_ptr(),
            need_fullpath: false,
            block_extract: false,
        };
    }
}

/// # Safety
///
/// `info` must be a valid pointer to a `retro_system_av_info`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn retro_get_system_av_info(info: *mut RetroSystemAvInfo) {
    if info.is_null() {
        return;
    }

    let av_info =
        AV_INFO.lock().unwrap_or_else(PoisonError::into_inner).unwrap_or(RetroSystemAvInfo {
            geometry: ffi::RetroGameGeometry {
                base_width: 320,
                base_height: 224,
                max_width: 320,
                max_height: 224,
                aspect_ratio: 4.0 / 3.0,
            },
            timing: ffi::RetroSystemTiming {
                fps: 60.0,
                sample_rate: session::DEFAULT_SAMPLE_RATE.into(),
            },
        });

    // SAFETY: Checked non-null above; frontend guarantees validity
    unsafe {
        *info = av_info;
    }
}

/// # Safety
///
/// `cb` must be a valid environment callback.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn retro_set_environment(cb: Option<ffi::RetroEnvironmentFn>) {
    env::set_environment(cb);

    guard("retro_set_environment", (), || {
        vfs::init();

        let overrides = [
            RetroSystemContentInfoOverride {
                extensions: detect::DISC_EXTENSIONS.as_ptr(),
                need_fullpath: true,
                persistent_data: false,
            },
            RetroSystemContentInfoOverride {
                extensions: ptr::null(),
                need_fullpath: false,
                persistent_data: false,
            },
        ];
        // SAFETY: SET_CONTENT_INFO_OVERRIDE takes a NULL-terminated array
        unsafe {
            env::environment(
                ffi::RETRO_ENVIRONMENT_SET_CONTENT_INFO_OVERRIDE,
                overrides.as_ptr().cast_mut().cast(),
            );
        }

        let mut support_no_game = true;
        // SAFETY: SET_SUPPORT_NO_GAME takes a const bool*
        unsafe {
            env::environment(
                ffi::RETRO_ENVIRONMENT_SET_SUPPORT_NO_GAME,
                (&raw mut support_no_game).cast(),
            );
        }

        register_subsystems();
        options::register();
    });
}

/// `retro_load_game_special` game type for a cartridge booted with a Sega CD disc
const SUBSYSTEM_SEGA_CD_MODE_1: c_uint = 1;

fn register_subsystems() {
    let roms = [
        ffi::RetroSubsystemRomInfo {
            desc: c"Cartridge".as_ptr(),
            valid_extensions: c"gen|md|bin|smd|32x".as_ptr(),
            need_fullpath: false,
            block_extract: false,
            required: true,
            memory: ptr::null(),
            num_memory: 0,
        },
        ffi::RetroSubsystemRomInfo {
            desc: c"Disc".as_ptr(),
            valid_extensions: c"cue|chd".as_ptr(),
            need_fullpath: true,
            block_extract: true,
            required: true,
            memory: ptr::null(),
            num_memory: 0,
        },
    ];
    let subsystems = [
        ffi::RetroSubsystemInfo {
            desc: c"Sega CD Mode 1 (Cartridge + Disc)".as_ptr(),
            ident: c"scd_mode1".as_ptr(),
            roms: roms.as_ptr(),
            num_roms: roms.len() as c_uint,
            id: SUBSYSTEM_SEGA_CD_MODE_1,
        },
        ffi::RetroSubsystemInfo {
            desc: ptr::null(),
            ident: ptr::null(),
            roms: ptr::null(),
            num_roms: 0,
            id: 0,
        },
    ];
    // SAFETY: SET_SUBSYSTEM_INFO takes a zero-terminated retro_subsystem_info array, which the
    // frontend copies
    unsafe {
        env::environment(
            ffi::RETRO_ENVIRONMENT_SET_SUBSYSTEM_INFO,
            subsystems.as_ptr().cast_mut().cast(),
        );
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn retro_set_video_refresh(cb: Option<ffi::RetroVideoRefreshFn>) {
    env::set_video_refresh(cb);
}

#[unsafe(no_mangle)]
pub extern "C" fn retro_set_audio_sample(_cb: Option<ffi::RetroAudioSampleFn>) {}

#[unsafe(no_mangle)]
pub extern "C" fn retro_set_audio_sample_batch(cb: Option<ffi::RetroAudioSampleBatchFn>) {
    env::set_audio_sample_batch(cb);
}

#[unsafe(no_mangle)]
pub extern "C" fn retro_set_input_poll(cb: Option<ffi::RetroInputPollFn>) {
    env::set_input_poll(cb);
}

#[unsafe(no_mangle)]
pub extern "C" fn retro_set_input_state(cb: Option<ffi::RetroInputStateFn>) {
    env::set_input_state(cb);
}

#[unsafe(no_mangle)]
pub extern "C" fn retro_init() {
    guard("retro_init", (), || {
        env::init_frontend_interfaces();
        env::set_serialization_quirks();
        if vfs::is_available() {
            log::info!("Using the frontend's virtual file system");
        }
        panic::set_hook(Box::new(|info| log::error!("{info}")));
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn retro_deinit() {
    guard("retro_deinit", (), || {
        core().game = None;
        set_av_info(None);
    });
}

/// Path and data from a `retro_game_info`.
///
/// # Safety
///
/// `game` must be a valid `retro_game_info`.
unsafe fn game_path_and_data(game: &RetroGameInfo) -> (Option<String>, Option<&[u8]>) {
    let path = (!game.path.is_null())
        // SAFETY: Frontend provides a NUL-terminated path string
        .then(|| unsafe { CStr::from_ptr(game.path) }.to_string_lossy().into_owned());
    let data = (!game.data.is_null() && game.size != 0)
        // SAFETY: Frontend provides a buffer of game.size bytes
        .then(|| unsafe { slice::from_raw_parts(game.data.cast::<u8>(), game.size) });
    (path, data)
}

/// Like [`guard`], but a panic also unloads the game: the emulator's state can't be trusted after
/// one, so stop running it.
fn guard_game<T>(name: &str, default: T, f: impl FnOnce() -> T) -> T {
    let mut panicked = true;
    let result = guard(name, default, || {
        let result = f();
        panicked = false;
        result
    });
    if panicked {
        guard("unloading crashed game", (), || core().game = None);
        env::show_message("jgenesis: the emulator crashed; see the log for details", 600);
    }
    result
}

/// Shared by `retro_load_game` and `retro_load_game_special`.
fn start_game(name: &str, load: impl FnOnce() -> Result<LoadedGame, String>) -> bool {
    guard(name, false, || {
        let mut pixel_format = ffi::RETRO_PIXEL_FORMAT_XRGB8888;
        // SAFETY: SET_PIXEL_FORMAT takes a const enum retro_pixel_format*
        let ok = unsafe {
            env::environment(
                ffi::RETRO_ENVIRONMENT_SET_PIXEL_FORMAT,
                (&raw mut pixel_format).cast(),
            )
        };
        if !ok {
            log::error!("Frontend does not support XRGB8888 pixel format");
            return false;
        }

        let loaded = match load() {
            Ok(loaded) => loaded,
            Err(err) => {
                log::error!("{err}");
                env::show_message(&err, 300);
                return false;
            }
        };

        if matches!(loaded.console, Console::SegaCd | Console::SegaCd32X | Console::PcEngineCd) {
            register_disk_control();
        }

        set_av_info(Some(loaded.session.av_info()));
        // With no disc to start with (running the Sega CD BIOS), the drive starts open
        let disc_ejected = loaded.discs.is_empty();
        core().game = Some(Game { loaded, disc_index: 0, disc_ejected });
        env::negotiate_float_audio();

        true
    })
}

/// # Safety
///
/// `game` must be NULL or a valid pointer to a `retro_game_info`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn retro_load_game(game: *const RetroGameInfo) -> bool {
    if game.is_null() {
        return start_game("retro_load_game", load::load_no_game);
    }

    // SAFETY: Checked non-null above; frontend guarantees validity for the duration of this call
    let (path, data) = unsafe { game_path_and_data(&*game) };
    start_game("retro_load_game", || load::load_game(path.as_deref(), data))
}

/// # Safety
///
/// `info` must be a valid pointer to `num_info` `retro_game_info`s.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn retro_load_game_special(
    game_type: c_uint,
    info: *const RetroGameInfo,
    num_info: usize,
) -> bool {
    if game_type != SUBSYSTEM_SEGA_CD_MODE_1 || info.is_null() || num_info != 2 {
        log::error!("Unsupported subsystem {game_type} with {num_info} content files");
        return false;
    }

    // SAFETY: Checked non-null above; frontend provides num_info entries
    let info = unsafe { slice::from_raw_parts(info, num_info) };
    // SAFETY: Frontend guarantees validity for the duration of this call
    let (cartridge_path, cartridge_data) = unsafe { game_path_and_data(&info[0]) };
    // SAFETY: As above
    let (disc_path, _) = unsafe { game_path_and_data(&info[1]) };

    start_game("retro_load_game_special", || {
        load::load_sega_cd_mode_1(cartridge_path.as_deref(), cartridge_data, disc_path.as_deref())
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn retro_unload_game() {
    guard("retro_unload_game", (), || {
        if let Some(mut game) = core().game.take() {
            game.loaded.session.flush_saves();
        }
        env::clear_float_audio();
        set_av_info(None);
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn retro_run() {
    guard_game("retro_run", (), || {
        let mut core = core();
        let Some(game) = &mut core.game else {
            env::input_poll();
            env::video_refresh(None, 0, 0);
            return;
        };

        if env::variables_updated() {
            game.loaded.session.apply_options(&Options::read());
        }

        if let Err(err) = game.loaded.session.run_frame() {
            log::error!("Emulator error: {err}");
        }

        let av_info = game.loaded.session.av_info();
        drop(core);
        set_av_info(Some(av_info));
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn retro_reset() {
    guard_game("retro_reset", (), || {
        if let Some(game) = &mut core().game {
            game.loaded.session.reset();
        }
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn retro_set_controller_port_device(port: c_uint, device: c_uint) {
    guard("retro_set_controller_port_device", (), || {
        if let Some(game) = &mut core().game {
            game.loaded.session.set_controller(port as usize, device);
        }
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn retro_serialize_size() -> usize {
    guard("retro_serialize_size", 0, || {
        core().game.as_mut().map_or(0, |game| game.loaded.session.serialize_size())
    })
}

/// # Safety
///
/// `data` must point to a writable buffer of `size` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn retro_serialize(data: *mut c_void, size: usize) -> bool {
    if data.is_null() {
        return false;
    }

    // SAFETY: Frontend provides a writable buffer of `size` bytes
    let out = unsafe { slice::from_raw_parts_mut(data.cast::<u8>(), size) };
    guard_game("retro_serialize", false, || {
        core().game.as_mut().is_some_and(|game| game.loaded.session.serialize(out))
    })
}

/// # Safety
///
/// `data` must point to a readable buffer of `size` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn retro_unserialize(data: *const c_void, size: usize) -> bool {
    if data.is_null() {
        return false;
    }

    // SAFETY: Frontend provides a readable buffer of `size` bytes
    let data = unsafe { slice::from_raw_parts(data.cast::<u8>(), size) };
    guard_game("retro_unserialize", false, || {
        let mut core = core();
        let Some(game) = &mut core.game else { return false };
        match game.loaded.session.unserialize(data) {
            Ok(()) => true,
            Err(err) => {
                log::error!("{err}");
                env::show_message(&err, 300);
                false
            }
        }
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn retro_cheat_reset() {}

#[unsafe(no_mangle)]
pub extern "C" fn retro_cheat_set(_index: c_uint, _enabled: bool, _code: *const c_char) {}

#[unsafe(no_mangle)]
pub extern "C" fn retro_get_region() -> c_uint {
    let fps =
        AV_INFO.lock().unwrap_or_else(PoisonError::into_inner).map_or(60.0, |av| av.timing.fps);
    if fps < 55.0 { ffi::RETRO_REGION_PAL } else { ffi::RETRO_REGION_NTSC }
}

/// Save RAM is written directly to the save directory instead (see `frontend::SaveFiles`)
#[unsafe(no_mangle)]
pub extern "C" fn retro_get_memory_data(_id: c_uint) -> *mut c_void {
    ptr::null_mut()
}

#[unsafe(no_mangle)]
pub extern "C" fn retro_get_memory_size(_id: c_uint) -> usize {
    0
}

// Disc control

fn register_disk_control() {
    let mut callback = ffi::RetroDiskControlCallback {
        set_eject_state: disk_set_eject_state,
        get_eject_state: disk_get_eject_state,
        get_image_index: disk_get_image_index,
        set_image_index: disk_set_image_index,
        get_num_images: disk_get_num_images,
        replace_image_index: disk_replace_image_index,
        add_image_index: disk_add_image_index,
    };
    // SAFETY: SET_DISK_CONTROL_INTERFACE takes a const retro_disk_control_callback*
    unsafe {
        env::environment(
            ffi::RETRO_ENVIRONMENT_SET_DISK_CONTROL_INTERFACE,
            (&raw mut callback).cast(),
        );
    }
}

fn with_game<T: Copy>(name: &str, default: T, f: impl FnOnce(&mut Game) -> T) -> T {
    guard_game(name, default, || core().game.as_mut().map_or(default, f))
}

extern "C" fn disk_set_eject_state(ejected: bool) -> bool {
    with_game("disk_set_eject_state", false, |game| {
        if ejected == game.disc_ejected {
            return true;
        }

        let disc = if ejected {
            None
        } else {
            match game.loaded.discs.get(game.disc_index) {
                Some(path) if !path.as_os_str().is_empty() => match load::open_disc(path) {
                    Ok(disc) => Some(disc),
                    Err(err) => {
                        log::error!("{err}");
                        return false;
                    }
                },
                // Index past the end means "no disc"
                _ => None,
            }
        };

        match game.loaded.session.change_disc(disc) {
            Ok(()) => {
                game.disc_ejected = ejected;
                true
            }
            Err(err) => {
                log::error!("Unable to change disc: {err}");
                false
            }
        }
    })
}

extern "C" fn disk_get_eject_state() -> bool {
    with_game("disk_get_eject_state", false, |game| game.disc_ejected)
}

extern "C" fn disk_get_image_index() -> c_uint {
    with_game("disk_get_image_index", 0, |game| game.disc_index as c_uint)
}

extern "C" fn disk_set_image_index(index: c_uint) -> bool {
    with_game("disk_set_image_index", false, |game| {
        if !game.disc_ejected || index as usize > game.loaded.discs.len() {
            return false;
        }
        game.disc_index = index as usize;
        true
    })
}

extern "C" fn disk_get_num_images() -> c_uint {
    with_game("disk_get_num_images", 0, |game| game.loaded.discs.len() as c_uint)
}

extern "C" fn disk_replace_image_index(index: c_uint, info: *const RetroGameInfo) -> bool {
    let path = if info.is_null() {
        None
    } else {
        // SAFETY: Frontend provides a valid retro_game_info with a NUL-terminated path
        let path = unsafe { (*info).path };
        if path.is_null() {
            return false;
        }
        // SAFETY: As above
        Some(PathBuf::from(unsafe { CStr::from_ptr(path) }.to_string_lossy().into_owned()))
    };

    with_game("disk_replace_image_index", false, |game| {
        let index = index as usize;
        if index >= game.loaded.discs.len() {
            return false;
        }

        match path {
            Some(path) => game.loaded.discs[index] = path,
            None => {
                game.loaded.discs.remove(index);
                if game.disc_index > index {
                    game.disc_index -= 1;
                }
            }
        }
        true
    })
}

extern "C" fn disk_add_image_index() -> bool {
    with_game("disk_add_image_index", false, |game| {
        game.loaded.discs.push(PathBuf::new());
        true
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info_value(key: &str) -> &'static str {
        include_str!("../jgenesis_libretro.info")
            .lines()
            .find_map(|line| {
                line.strip_prefix(key)?
                    .trim()
                    .strip_prefix('=')?
                    .trim()
                    .strip_prefix('"')?
                    .strip_suffix('"')
            })
            .unwrap()
    }

    #[test]
    fn info_file_matches_core() {
        assert_eq!(info_value("supported_extensions"), detect::VALID_EXTENSIONS.to_str().unwrap());
        assert_eq!(info_value("display_version"), LIBRARY_VERSION.to_str().unwrap());
    }
}
