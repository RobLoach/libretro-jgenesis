# libretro-jgenesis

A [libretro](https://www.libretro.com/) core for [jgenesis](https://github.com/jsgroth/jgenesis), a cycle-accurate multi-system emulator written in Rust. It runs in RetroArch and other libretro frontends.

One core covers every system jgenesis emulates:

| System | Extensions | Firmware (in the frontend's system directory) |
|---|---|---|
| Sega Genesis / Mega Drive | `gen` `md` `bin` `smd` | — |
| Sega CD / Mega CD | `cue` `chd` `m3u` | `bios_CD_U.bin`, `bios_CD_E.bin`, `bios_CD_J.bin` (matching the disc's region) |
| Sega 32X, Sega CD 32X | `32x` `bin`, `cue` `chd` | 32X: none. Sega CD 32X: Sega CD BIOS |
| Master System, Game Gear, SG-1000 | `sms` `gg` `sg` | — |
| NES / Famicom | `nes` | — |
| SNES / Super Famicom | `sfc` `smc` | `dsp1.rom`–`dsp4.rom`, `st010.rom`, `st011.rom`, `st018.rom`, only for games that use those chips |
| Game Boy, Game Boy Color | `gb` `gbc` | — |
| Game Boy Advance | `gba` `bin` | `gba_bios.bin` (required) |
| PC Engine / TurboGrafx-16 | `pce` | — |
| PC Engine CD / TurboGrafx-CD | `cue` `chd` | `syscard3.pce` |

The console is detected from the file extension and contents: Genesis/32X/Sega CD headers, GBA headers for `.bin`, and Sega CD vs. PC Engine CD for disc images.

## Using it

1. Build the core (below), or download a build from the [Actions](../../actions) tab.
2. Copy `jgenesis_libretro.so` / `.dll` / `.dylib` into RetroArch's `cores` directory and `jgenesis_libretro.info` into its `info` directory.
3. Load content with *Load Core → jgenesis*.

Core options are grouped by system under *Quick Menu → Core Options*. They cover region, timing, aspect ratio, sprite limits, borders, overclocking, palettes, LCD color correction and frame blending.

### Saves

Battery saves are written to the frontend's save directory as `<game>.<ext>`, using the same file names and formats as standalone jgenesis, so you can move saves between the two. The core writes these files itself, because several systems keep more than one save file (Sega CD backup RAM and RAM cartridge, GBA RTC). This means libretro's single `SAVE_RAM` buffer isn't used, and frontend features built on it (e.g. netplay SRAM sync) won't see the saves.

Save states and rewind are supported. Save states are tied to the jgenesis version the core was built from.

### Controls

All systems use the RetroPad. Genesis follows Genesis Plus GX's layout: Y/B/A are A/B/C, L/X/R are X/Y/Z, and Select is Mode. Each Genesis port can be set to a 6-button pad (the default), a 3-button pad, or nothing. PC Engine follows Beetle PCE's layout: A/B are I/II. The TurboTap core option enables 5 players.

## Building

Requires Rust 1.99.1 or newer, pinned in `rust-toolchain.toml`, and a C compiler for the zstd library used to read CHD images.

```sh
make -f Makefile.libretro                # host platform
cargo test --release                     # unit tests + a smoke test that loads the built core
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

To test against real content, set `JGENESIS_TEST_ROMS` (paths separated by `;`) and `JGENESIS_TEST_SYSTEM_DIR` (a BIOS directory) before running `cargo test --release`.

### CI

- **GitHub Actions** (`.github/workflows/build.yml`) runs tests and builds every platform above.
- **`.gitlab-ci.yml`** is laid out for libretro's buildbot templates. Those images only ship C toolchains, so it sets `RUST_BOOTSTRAP=1`, which makes `Makefile.libretro` install rustup before building.

### Unsupported platforms

PS2, PS3, GameCube, Wii, Wii U and Switch have no Rust target with a standard library. PSP, Vita, 3DS and the MIPS handhelds have Rust targets, but their CPUs are far too slow for jgenesis's cycle-accurate emulation. 32-bit iOS and the MSVC 2005/2010 toolchains aren't supported by Rust.

### Performance

jgenesis favors accuracy over speed. 32X, Sega CD, and SNES games with coprocessors need a reasonably fast CPU, and may not run at full speed on low-end Android devices or in the browser.

## How it works

The jgenesis backend crates are used unmodified, as git dependencies pinned to a specific revision. The libretro API is a small hand-written FFI layer (`src/ffi.rs`); there's no bindgen dependency, so it cross-compiles without libclang. Every emulator implements jgenesis's `EmulatorTrait`, so one generic `Session<E>` (`src/session.rs`) handles video, audio, input, saves and save states for all systems. The GPU post-processing that standalone jgenesis does in shaders (GBC/GBA LCD color correction and frame blending) is reimplemented on the CPU (`src/frontend.rs`).

## License

GPL-3.0-or-later, the same as jgenesis.
