//! libretro core for [jgenesis](https://github.com/jsgroth/jgenesis).

mod detect;
mod env;
mod ffi;
mod frontend;
mod load;
mod options;
mod session;
mod systems;

use detect::Console;
use ffi::{
    RetroControllerDescription, RetroControllerInfo, RetroGameInfo, RetroInputDescriptor,
    RetroSystemAvInfo, RetroSystemContentInfoOverride, RetroSystemInfo,
};
use load::LoadedGame;
use options::Options;
use std::any::Any;
use std::ffi::{CStr, c_char, c_uint, c_void};
use std::panic::{self, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::{ptr, slice};

const LIBRARY_NAME: &CStr = c"jgenesis";
const LIBRARY_VERSION: &CStr = c"0.14.1 (libretro 0.1.0)";
const VALID_EXTENSIONS: &CStr =
    c"sg|sms|gg|gen|md|bin|smd|32x|nes|sfc|smc|gb|gbc|gba|pce|cue|chd|m3u";
const FULLPATH_EXTENSIONS: &CStr = c"cue|chd|m3u";

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
            valid_extensions: VALID_EXTENSIONS.as_ptr(),
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
                sample_rate: session::AUDIO_SAMPLE_RATE.into(),
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
        let overrides = [
            RetroSystemContentInfoOverride {
                extensions: FULLPATH_EXTENSIONS.as_ptr(),
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

        options::register();
    });
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

fn input_descriptors(console: Console) -> (usize, &'static [(c_uint, &'static CStr)]) {
    use ffi::{
        RETRO_DEVICE_ID_JOYPAD_A as A, RETRO_DEVICE_ID_JOYPAD_B as B,
        RETRO_DEVICE_ID_JOYPAD_DOWN as DOWN, RETRO_DEVICE_ID_JOYPAD_L as L,
        RETRO_DEVICE_ID_JOYPAD_LEFT as LEFT, RETRO_DEVICE_ID_JOYPAD_R as R,
        RETRO_DEVICE_ID_JOYPAD_RIGHT as RIGHT, RETRO_DEVICE_ID_JOYPAD_SELECT as SELECT,
        RETRO_DEVICE_ID_JOYPAD_START as START, RETRO_DEVICE_ID_JOYPAD_UP as UP,
        RETRO_DEVICE_ID_JOYPAD_X as X, RETRO_DEVICE_ID_JOYPAD_Y as Y,
    };

    const DPAD: [(c_uint, &CStr); 4] =
        [(UP, c"Up"), (DOWN, c"Down"), (LEFT, c"Left"), (RIGHT, c"Right")];

    macro_rules! with_dpad {
        ($($id:expr => $label:literal),* $(,)?) => {
            &[DPAD[0], DPAD[1], DPAD[2], DPAD[3], $(($id, $label)),*]
        };
    }

    match console {
        Console::Genesis | Console::SegaCd | Console::Sega32X | Console::SegaCd32X => (
            2,
            with_dpad![Y => c"A", B => c"B", A => c"C", L => c"X", X => c"Y", R => c"Z",
                START => c"Start", SELECT => c"Mode"],
        ),
        Console::Sg1000 | Console::MasterSystem | Console::GameGear => {
            (2, with_dpad![B => c"Button 1", A => c"Button 2", START => c"Start / Pause"])
        }
        Console::Nes => {
            (2, with_dpad![A => c"A", B => c"B", START => c"Start", SELECT => c"Select"])
        }
        Console::Snes => (
            2,
            with_dpad![A => c"A", B => c"B", X => c"X", Y => c"Y", L => c"L", R => c"R",
                START => c"Start", SELECT => c"Select"],
        ),
        Console::GameBoy => {
            (1, with_dpad![A => c"A", B => c"B", START => c"Start", SELECT => c"Select"])
        }
        Console::GameBoyAdvance => (
            1,
            with_dpad![A => c"A", B => c"B", L => c"L", R => c"R", START => c"Start",
                SELECT => c"Select"],
        ),
        Console::PcEngine | Console::PcEngineCd => (
            5,
            with_dpad![A => c"I", B => c"II", Y => c"III", X => c"IV", L => c"V", R => c"VI",
                START => c"Run", SELECT => c"Select"],
        ),
    }
}

/// Advertise the controller types for each of the loaded system's ports. The frontend keeps its
/// own copy of the arrays.
fn set_controller_info(console: Console) {
    let (ports, _) = input_descriptors(console);

    let types: Vec<_> = match console {
        // Only Genesis-family ports can change controller type (see `System::set_controller`)
        Console::Genesis | Console::SegaCd | Console::Sega32X | Console::SegaCd32X => vec![
            RetroControllerDescription {
                desc: c"6-Button Pad".as_ptr(),
                id: ffi::RETRO_DEVICE_JOYPAD,
            },
            RetroControllerDescription {
                desc: c"3-Button Pad".as_ptr(),
                id: systems::GENESIS_DEVICE_3_BUTTON,
            },
            RetroControllerDescription { desc: c"None".as_ptr(), id: ffi::RETRO_DEVICE_NONE },
        ],
        _ => vec![RetroControllerDescription {
            desc: c"Gamepad".as_ptr(),
            id: ffi::RETRO_DEVICE_JOYPAD,
        }],
    };

    let infos: Vec<_> = (0..ports)
        .map(|_| RetroControllerInfo { types: types.as_ptr(), num_types: types.len() as c_uint })
        .chain([RetroControllerInfo { types: ptr::null(), num_types: 0 }])
        .collect();

    // SAFETY: SET_CONTROLLER_INFO takes a NULL-terminated retro_controller_info array
    unsafe {
        env::environment(
            ffi::RETRO_ENVIRONMENT_SET_CONTROLLER_INFO,
            infos.as_ptr().cast_mut().cast(),
        );
    }
}

fn set_input_descriptors(console: Console) {
    let (ports, buttons) = input_descriptors(console);

    let descriptors: Vec<_> = (0..ports as c_uint)
        .flat_map(|port| {
            buttons.iter().map(move |&(id, label)| RetroInputDescriptor {
                port,
                device: ffi::RETRO_DEVICE_JOYPAD,
                index: 0,
                id,
                description: label.as_ptr(),
            })
        })
        .chain([RetroInputDescriptor {
            port: 0,
            device: 0,
            index: 0,
            id: 0,
            description: ptr::null(),
        }])
        .collect();

    // SAFETY: SET_INPUT_DESCRIPTORS takes a NULL-terminated retro_input_descriptor array
    unsafe {
        env::environment(
            ffi::RETRO_ENVIRONMENT_SET_INPUT_DESCRIPTORS,
            descriptors.as_ptr().cast_mut().cast(),
        );
    }
}

/// # Safety
///
/// `game` must be NULL or a valid pointer to a `retro_game_info`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn retro_load_game(game: *const RetroGameInfo) -> bool {
    if game.is_null() {
        log::error!("jgenesis requires content to be loaded");
        return false;
    }

    // SAFETY: Checked non-null above; frontend guarantees validity for the duration of this call
    let game = unsafe { &*game };
    let path = (!game.path.is_null())
        // SAFETY: Frontend provides a NUL-terminated path string
        .then(|| unsafe { CStr::from_ptr(game.path) }.to_string_lossy().into_owned());
    let data = (!game.data.is_null() && game.size != 0)
        // SAFETY: Frontend provides a buffer of game.size bytes
        .then(|| unsafe { slice::from_raw_parts(game.data.cast::<u8>(), game.size) });

    guard("retro_load_game", false, || {
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

        let loaded = match load::load_game(path.as_deref(), data) {
            Ok(loaded) => loaded,
            Err(err) => {
                log::error!("{err}");
                env::show_message(&err, 300);
                return false;
            }
        };

        set_input_descriptors(loaded.console);
        set_controller_info(loaded.console);
        if matches!(loaded.console, Console::SegaCd | Console::SegaCd32X | Console::PcEngineCd) {
            register_disk_control();
        }

        set_av_info(Some(loaded.session.av_info()));
        core().game = Some(Game { loaded, disc_index: 0, disc_ejected: false });

        true
    })
}

/// # Safety
///
/// Unsupported; always returns false.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn retro_load_game_special(
    _game_type: c_uint,
    _info: *const RetroGameInfo,
    _num_info: usize,
) -> bool {
    false
}

#[unsafe(no_mangle)]
pub extern "C" fn retro_unload_game() {
    guard("retro_unload_game", (), || {
        if let Some(mut game) = core().game.take() {
            game.loaded.session.flush_saves();
        }
        set_av_info(None);
    });
}

#[unsafe(no_mangle)]
pub extern "C" fn retro_run() {
    let ok = guard("retro_run", false, || {
        let mut core = core();
        let Some(game) = &mut core.game else {
            env::input_poll();
            env::video_refresh(None, 0, 0);
            return true;
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
        true
    });

    if !ok {
        // The emulator panicked; its state can't be trusted, so stop running it
        core().game = None;
        env::show_message("jgenesis: the emulator crashed; see the log for details", 600);
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn retro_reset() {
    guard("retro_reset", (), || {
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
    guard("retro_serialize", false, || {
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
    guard("retro_unserialize", false, || {
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

fn with_game<T>(name: &str, default: T, f: impl FnOnce(&mut Game) -> T) -> T {
    guard(name, default, || match &mut core().game {
        Some(game) => f(game),
        None => {
            panic::resume_unwind(Box::new("no game loaded"));
        }
    })
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
