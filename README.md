# libretro-jgenesis

A [libretro](https://www.libretro.com/) core for [jgenesis](https://github.com/jsgroth/jgenesis), the multi-console emulator by [jsgroth](https://github.com/jsgroth).

## Systems

| System | Extensions | Firmware |
|---|---|---|
| Sega Genesis / Mega Drive | `gen` `md` `bin` `smd` | None |
| Sega CD / Mega CD | `cue` `chd` `m3u` | `bios_CD_U.bin`, `bios_CD_E.bin`, `bios_CD_J.bin` |
| Sega 32X | `32x` `bin` | None |
| Sega CD 32X | `cue` `chd` `m3u` | Sega CD BIOS |
| Master System, Game Gear, SG-1000 | `sms` `gg` `sg` | None |
| NES / Famicom | `nes` | None |
| SNES / Super Famicom | `sfc` `smc` | `dsp1.rom`-`dsp4.rom`, `st010.rom`, `st011.rom`, `st018.rom` for coprocessor games |
| Game Boy, Game Boy Color | `gb` `gbc` | None |
| Game Boy Advance | `gba` `bin` | `gba_bios.bin` |
| PC Engine / TurboGrafx-16 | `pce` | None |
| PC Engine CD / TurboGrafx-CD | `cue` `chd` `m3u` | `syscard3.pce` |

## Installation

1. Build the core (see [Development](#development)), or download a build
2. Copy `jgenesis_libretro.so` / `.dll` / `.dylib` into RetroArch's `cores` directory
3. Copy [`jgenesis_libretro.info`](jgenesis_libretro.info) into RetroArch's `info` directory
4. Put firmware files in RetroArch's `system` directory
5. Load content with *Load Core → jgenesis*

## Usage

Some additional points around using libretro-jgenesis...

### System Selection

The system is picked from the file extension, then refined from the contents:
- `.bin` is a GBA ROM if it has a GBA header, otherwise Genesis
- Genesis ROM headers identify 32X and Sega CD-capable cartridges
- Disc images are identified as Sega CD, Sega CD 32X, or PC Engine CD

### Discs

- **Formats:** load discs as `.cue`, `.chd`, or `.m3u` playlist for multi-disc games
- **Disc swapping:** works through RetroArch's *Disc Control* menu
- **Zipped CUE/BIN images don't work.** RetroArch only extracts or loads one file from the archive, so the BIN tracks are missing. Unzip them, or convert to CHD.

### Sega CD Mode 1

The *Sega CD Mode 1 (Cartridge + Disc)* subsystem boots a Genesis or 32X cartridge with the Sega CD attached and a disc inserted.

From the command line:

```sh
retroarch -L jgenesis_libretro.so --subsystem=scd_mode1 game.md disc.cue
```

### Saves

The core writes battery saves itself:
- **File names:** `<game>.sav`, plus `.rtc` for cartridges with a clock and `.ramc` for the Sega CD RAM cartridge.
- **Location:** saves go in the save directory RetroArch provides. RetroArch's *sort by core*, *sort by content directory*, and *save files in content directory* settings all apply.

See [#8](https://github.com/RobLoach/libretro-jgenesis/issues/8) for improvements for `SAVE_RAM` memory.

### Frontend features

- **File access:** goes through the frontend's virtual file system (VFS) when it has one
  - CUE/BIN discs are still read directly from disk, so they don't load from `smb://` / `nfs://` shares or other VFS-only locations yet ([#12](https://github.com/RobLoach/libretro-jgenesis/issues/12))
  - With a VFS, CHD images are read fully into memory
- **Audio:** goes out at the frontend's output sample rate, as float samples when the frontend supports it
- **Run-ahead:** frames the frontend won't show skip video and audio conversion
- **Core options:** settings for each system have their own category

## Development

Requires the Rust toolchain pinned in `rust-toolchain.toml` (1.99.1), and a C compiler for the zstd library used to read CHD images.

```sh
make -f Makefile.libretro   # build jgenesis_libretro.so for the host platform
cargo test                  # unit tests, plus smoke tests that load the built core
```

`Makefile.libretro` maps libretro platform names to Rust targets:

| Platform | Command | Output |
|---|---|---|
| Linux x86_64 / i686 / aarch64 / armv7 | `make -f Makefile.libretro platform=unix ARCH=aarch64` | `jgenesis_libretro.so` |
| Windows (mingw / MSVC) | `platform=win64`, `platform=win32`, `platform=windows_msvc ARCH=x86_64` | `jgenesis_libretro.dll` |
| macOS x86_64 / arm64 | `platform=osx ARCH=arm64` | `jgenesis_libretro.dylib` |
| Android | `platform=android ABI=arm64-v8a NDK_ROOT=/path/to/ndk` | `jgenesis_libretro_android.so` |
| iOS | `platform=ios-arm64` | `jgenesis_libretro_ios.dylib` |
| tvOS | `platform=tvos-arm64` (nightly toolchain) | `jgenesis_libretro_tvos.dylib` |
| Emscripten | `platform=emscripten` (with emsdk) | `jgenesis_libretro_emscripten.a` |
| webOS | `platform=webos CROSS_COMPILE=...` | `jgenesis_libretro.so` |

For Linux cross builds without a cross toolchain, use [cargo-zigbuild](https://github.com/rust-cross/cargo-zigbuild): `make -f Makefile.libretro platform=unix ARCH=aarch64 CARGO_BUILD=zigbuild GLIBC=2.28`. `jni/` holds an `ndk-build` shim for libretro's Android CI template.

### Tests

The smoke tests (`tests/smoke.rs`) load the built core and drive it like a minimal frontend. That includes a test VFS and switchable frontend capabilities. A synthetic NES ROM is always tested. Environment variables add real content:

| Variable | Effect |
|---|---|
| `JGENESIS_TEST_ROMS` | `;`-separated content paths to load, run, and save-state |
| `JGENESIS_TEST_SYSTEM_DIR` | Firmware directory; also enables the no-content Sega CD BIOS tests |
| `JGENESIS_TEST_FRAMES` | Frames to run each ROM for (default 600) |
| `JGENESIS_TEST_PRESS_START` | Tap Start every few seconds, e.g. to get past the PC Engine System Card |
| `JGENESIS_TEST_DUMP_DIR` | Write each ROM's last frame there as a PPM image |
| `JGENESIS_TEST_SCD_MODE_1` | `<cartridge>;<disc>` to load through the Sega CD Mode 1 subsystem |

### CI

- **GitHub Actions** (`.github/workflows/build.yml`) runs the tests and builds every platform above
- **`.gitlab-ci.yml`** is laid out for libretro's buildbot templates

### Unsupported platforms

PS2, PS3, GameCube, Wii, Wii U and Switch have no Rust target with a standard library. PSP, Vita, 3DS and the MIPS handhelds have Rust targets, but their CPUs are far too slow for jgenesis's cycle-accurate emulation. 32-bit iOS and the MSVC 2005/2010 toolchains aren't supported by Rust.

### Performance

jgenesis favors accuracy over speed. 32X, Sega CD, and SNES games with coprocessors need a reasonably fast CPU, and may not run at full speed on low-end Android devices or in the browser.

### Design

- **Upstream code:** the jgenesis backend crates are used unmodified, as git dependencies pinned to a specific revision.
- **libretro API:** a small hand-written FFI layer (`src/ffi.rs`). There's no bindgen dependency, so the core cross-compiles without libclang.
- **One session type for every system:** every emulator implements jgenesis's `EmulatorTrait`, so one generic `Session<E>` (`src/session.rs`) handles video, audio, input, saves, and save states for all of them.
- **CPU post-processing:** standalone jgenesis does GBC/GBA LCD color correction and frame blending in GPU shaders. Here they're reimplemented on the CPU (`src/frontend.rs`).
- **File access:** goes through `src/vfs.rs`, which uses the frontend's VFS and falls back to `std::fs`.

## License

[GPL-3.0-or-later](LICENSE), the same as [jgenesis](https://github.com/jsgroth/jgenesis/blob/master/LICENSE).
