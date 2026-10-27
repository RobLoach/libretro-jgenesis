//! Implementations of the jgenesis frontend traits ([`Renderer`], [`AudioOutput`], [`SaveWriter`])
//! that buffer output for the libretro callbacks.

use bincode::{Decode, Encode};
use jgenesis_common::frontend::{
    AudioOutput, Color, ColorCorrection, FrameSize, RenderFrameOptions, Renderer, SaveWriter,
};
use std::collections::HashMap;
use std::convert::Infallible;
use std::fmt::{Display, Formatter};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::{fs, io};

/// Same bincode configuration as the standalone jgenesis frontend, so that serialized save files
/// (e.g. RTC state) are interchangeable between the two.
pub fn bincode_config() -> impl bincode::config::Config {
    bincode::config::standard()
        .with_little_endian()
        .with_fixed_int_encoding()
        .with_limit::<{ 100 * 1024 * 1024 }>()
}

/// Frame buffer in the frontend's XRGB8888 pixel format, plus the metadata needed to report
/// geometry.
#[derive(Default)]
pub struct VideoOutput {
    pub frame: Vec<u32>,
    pub size: Option<FrameSize>,
    pub pixel_aspect_ratio: Option<f64>,
    pub target_fps: f64,
    /// Set whenever the emulator renders a frame; cleared by the caller
    pub frame_rendered: bool,
    /// Previous frame before blending, for frame blending (which blends the current and previous
    /// emulator frames, not the current frame and the previous blended output)
    previous_raw_frame: Vec<u32>,
    raw_frame: Vec<u32>,
    correction: Option<(ColorCorrection, Box<CorrectionLut>)>,
}

impl Renderer for VideoOutput {
    type Err = Infallible;

    fn render_frame(
        &mut self,
        frame_buffer: &[Color],
        frame_size: FrameSize,
        target_fps: f64,
        options: RenderFrameOptions,
    ) -> Result<(), Self::Err> {
        let len = frame_size.len() as usize;
        let pixels = &frame_buffer[..len];

        // Without blending, convert straight into the output frame
        let target = if options.frame_blending { &mut self.raw_frame } else { &mut self.frame };
        target.clear();
        match options.color_correction {
            ColorCorrection::None => {
                target.extend(pixels.iter().map(|&color| to_xrgb8888(color)));
            }
            correction => {
                let lut = correction_lut(&mut self.correction, correction);
                target.extend(pixels.iter().map(|&color| lut.apply(color)));
            }
        }

        if options.frame_blending {
            self.frame.clear();
            self.frame.extend_from_slice(&self.raw_frame);
            if self.previous_raw_frame.len() == self.frame.len() {
                blend_frames(&mut self.frame, &self.previous_raw_frame);
            }
            std::mem::swap(&mut self.raw_frame, &mut self.previous_raw_frame);
        } else {
            self.previous_raw_frame.clear();
        }

        self.size = Some(frame_size);
        self.pixel_aspect_ratio = options.pixel_aspect_ratio.map(f64::from);
        self.target_fps = target_fps;
        self.frame_rendered = true;

        Ok(())
    }
}

fn correction_lut(
    cache: &mut Option<(ColorCorrection, Box<CorrectionLut>)>,
    correction: ColorCorrection,
) -> &CorrectionLut {
    if cache.as_ref().is_none_or(|(existing, _)| *existing != correction) {
        *cache = Some((correction, Box::new(CorrectionLut::new(correction))));
    }
    &cache.as_ref().unwrap().1
}

#[inline]
fn to_xrgb8888(Color { r, g, b, .. }: Color) -> u32 {
    (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b)
}

/// sRGB transfer function lookup tables. Blending and color correction are done in linear space
/// to match the GPU renderer in standalone jgenesis, which samples sRGB textures.
struct Srgb {
    to_linear: [f32; 256],
    from_linear: Vec<u8>,
}

const FROM_LINEAR_STEPS: usize = 4096;

static SRGB: LazyLock<Srgb> = LazyLock::new(|| {
    let to_linear = std::array::from_fn(|i| {
        let c = i as f32 / 255.0;
        if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    });

    let from_linear = (0..FROM_LINEAR_STEPS)
        .map(|i| {
            let l = i as f32 / (FROM_LINEAR_STEPS - 1) as f32;
            let c = if l <= 0.003_130_8 { l * 12.92 } else { 1.055 * l.powf(1.0 / 2.4) - 0.055 };
            (c * 255.0).round().clamp(0.0, 255.0) as u8
        })
        .collect();

    Srgb { to_linear, from_linear }
});

impl Srgb {
    #[inline]
    fn encode(&self, linear: f32) -> u8 {
        let i = (linear.clamp(0.0, 1.0) * (FROM_LINEAR_STEPS - 1) as f32).round() as usize;
        self.from_linear[i]
    }
}

fn blend_frames(frame: &mut [u32], previous: &[u32]) {
    let srgb = &*SRGB;
    let channel = |a: u32, b: u32, shift: u32| -> u32 {
        let a = srgb.to_linear[((a >> shift) & 0xFF) as usize];
        let b = srgb.to_linear[((b >> shift) & 0xFF) as usize];
        u32::from(srgb.encode(f32::midpoint(a, b))) << shift
    };

    for (pixel, &prev) in frame.iter_mut().zip(previous) {
        let cur = *pixel;
        *pixel = channel(cur, prev, 16) | channel(cur, prev, 8) | channel(cur, prev, 0);
    }
}

/// CPU port of jgenesis-renderer's `gb_color.wgsl` LCD color correction.
struct CorrectionLut {
    // Per-channel input value -> linear value with screen gamma applied
    gamma_linear: [f32; 256],
    // Row i = coefficients for output channel i
    matrix: [[f32; 3]; 3],
}

impl CorrectionLut {
    // Based on these public domain shaders:
    // https://github.com/libretro/common-shaders/blob/master/handheld/shaders/color/gbc-color.cg
    // https://github.com/libretro/common-shaders/blob/master/handheld/shaders/color/gba-color.cg
    const GBC: [[f32; 3]; 3] =
        [[0.78824, 0.12157, 0.0], [0.025, 0.72941, 0.275], [0.12039, 0.12157, 0.82]];
    const GBA: [[f32; 3]; 3] = [[0.845, 0.17, 0.015], [0.09, 0.68, 0.23], [0.16, 0.085, 0.755]];

    fn new(correction: ColorCorrection) -> Self {
        let (matrix, gamma) = match correction {
            ColorCorrection::GbcLcd { screen_gamma } => (Self::GBC, f32::from(screen_gamma)),
            ColorCorrection::GbaLcd { screen_gamma } => (Self::GBA, f32::from(screen_gamma)),
            ColorCorrection::None => ([[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]], 2.2),
        };

        let srgb = &*SRGB;
        let gamma_linear = std::array::from_fn(|i| srgb.to_linear[i].powf(gamma / 2.2));

        Self { gamma_linear, matrix }
    }

    #[inline]
    fn apply(&self, Color { r, g, b, .. }: Color) -> u32 {
        let rgb = [
            self.gamma_linear[r as usize],
            self.gamma_linear[g as usize],
            self.gamma_linear[b as usize],
        ];
        let srgb = &*SRGB;
        let out =
            self.matrix.map(|row| srgb.encode(row[0] * rgb[0] + row[1] * rgb[1] + row[2] * rgb[2]));
        (u32::from(out[0]) << 16) | (u32::from(out[1]) << 8) | u32::from(out[2])
    }
}

/// Interleaved signed 16-bit stereo samples.
#[derive(Default)]
pub struct AudioBuffer {
    pub samples: Vec<i16>,
}

impl AudioOutput for AudioBuffer {
    type Err = Infallible;

    #[inline]
    fn push_sample(&mut self, sample_l: f64, sample_r: f64) -> Result<(), Self::Err> {
        let convert = |sample: f64| (sample * f64::from(i16::MAX)).round() as i16;
        self.samples.push(convert(sample_l.clamp(-1.0, 1.0)));
        self.samples.push(convert(sample_r.clamp(-1.0, 1.0)));
        Ok(())
    }
}

#[derive(Debug)]
pub struct SaveError(String);

impl Display for SaveError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SaveError {}

/// Save files are written to `<save dir>/<content name>.<extension>`, matching the naming used by
/// standalone jgenesis. Writes are buffered in memory and flushed periodically and on unload.
pub struct SaveFiles {
    dir: PathBuf,
    base_name: String,
    pending: HashMap<String, Vec<u8>>,
}

impl SaveFiles {
    pub fn new(dir: PathBuf, base_name: String) -> Self {
        Self { dir, base_name, pending: HashMap::new() }
    }

    fn path(&self, extension: &str) -> PathBuf {
        self.dir.join(format!("{}.{extension}", self.base_name))
    }

    pub fn has_pending_writes(&self) -> bool {
        !self.pending.is_empty()
    }

    pub fn flush(&mut self) {
        if self.pending.is_empty() {
            return;
        }

        if let Err(err) = fs::create_dir_all(&self.dir) {
            log::error!("Unable to create save directory '{}': {err}", self.dir.display());
            return;
        }

        for (extension, bytes) in std::mem::take(&mut self.pending) {
            let path = self.path(&extension);
            match write_atomic(&path, &bytes) {
                Ok(()) => log::info!("Wrote save file '{}'", path.display()),
                Err(err) => log::error!("Unable to write save file '{}': {err}", path.display()),
            }
        }
    }
}

fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp_path = path.with_extension("tmp");
    fs::write(&tmp_path, bytes)?;
    fs::rename(&tmp_path, path)
}

impl SaveWriter for SaveFiles {
    type Err = SaveError;

    fn load_bytes(&mut self, extension: &str) -> Result<Vec<u8>, Self::Err> {
        if let Some(bytes) = self.pending.get(extension) {
            return Ok(bytes.clone());
        }

        let path = self.path(extension);
        fs::read(&path).map_err(|err| SaveError(format!("{}: {err}", path.display())))
    }

    fn persist_bytes(&mut self, extension: &str, bytes: &[u8]) -> Result<(), Self::Err> {
        self.pending.insert(extension.into(), bytes.to_vec());
        Ok(())
    }

    fn load_serialized<D: Decode<()>>(&mut self, extension: &str) -> Result<D, Self::Err> {
        let bytes = self.load_bytes(extension)?;
        let (value, _) = bincode::decode_from_slice(&bytes, bincode_config())
            .map_err(|err| SaveError(format!("Error decoding .{extension} save: {err}")))?;
        Ok(value)
    }

    fn persist_serialized<E: Encode>(&mut self, extension: &str, data: E) -> Result<(), Self::Err> {
        let bytes = bincode::encode_to_vec(data, bincode_config())
            .map_err(|err| SaveError(format!("Error encoding .{extension} save: {err}")))?;
        self.pending.insert(extension.into(), bytes);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(video: &mut VideoOutput, color: Color) {
        let options = RenderFrameOptions { frame_blending: true, ..RenderFrameOptions::default() };
        let size = FrameSize { width: 1, height: 1 };
        video.render_frame(&[color], size, 60.0, options).unwrap();
    }

    #[test]
    fn frame_blending_averages_only_the_last_two_frames() {
        let mut video = VideoOutput::default();
        let white = Color::rgb(255, 255, 255);

        // First frame has nothing to blend with
        render(&mut video, white);
        assert_eq!(video.frame, [0x00FF_FFFF]);

        // White + black averages to mid-grey (in linear light, so ~188 after sRGB encoding)
        render(&mut video, Color::BLACK);
        let grey = video.frame[0];
        assert!(grey != 0 && grey != 0x00FF_FFFF);

        // Two black frames in a row must be pure black: an older white frame must not linger
        render(&mut video, Color::BLACK);
        assert_eq!(video.frame, [0]);
    }
}
