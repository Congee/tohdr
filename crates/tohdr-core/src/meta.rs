//! Shared image and gain-map types.

/// Interleaved RGB, one `u16` per sample, values in `0..=(1 << bits) - 1`.
///
/// `u16` regardless of depth so 8- and 10-bit bases share one path; the encoder
/// narrows when it must.
#[derive(Clone, Debug)]
pub struct Rgb {
    pub width: u32,
    pub height: u32,
    pub bits: u8,
    pub data: Vec<u16>,
}

impl Rgb {
    pub fn max_value(&self) -> u16 {
        (1u32 << self.bits).saturating_sub(1) as u16
    }

    pub fn expected_len(&self) -> usize {
        self.width as usize * self.height as usize * 3
    }
}

/// The gain-map plane itself. Single channel: Apple's is monochrome, and ISO
/// 21496-1 permits 1 or 3. Often stored at half the base resolution.
#[derive(Clone, Debug)]
pub struct GainPlane {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

impl GainPlane {
    pub fn expected_len(&self) -> usize {
        self.width as usize * self.height as usize
    }
}

/// Flavor-neutral gain-map parameters.
///
/// Per-channel arrays are RGB; a monochrome map replicates one value across all
/// three. `*_log2` are base-2 log gains, matching ISO 21496-1 and libavif's
/// `gainMapMin`/`gainMapMax`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GainMapMeta {
    /// log2 gain applied where the map reads 0.
    pub min_log2: [f32; 3],
    /// log2 gain applied where the map reads max.
    pub max_log2: [f32; 3],
    pub gamma: [f32; 3],
    /// Added to base samples before applying gain, to keep 0 from pinning.
    pub base_offset: [f32; 3],
    /// Added to the reconstructed alternate samples.
    pub alt_offset: [f32; 3],
    /// log2 headroom of the base image. 0.0 for an SDR base.
    pub base_headroom: f32,
    /// log2 headroom once the gain map is fully applied.
    pub alt_headroom: f32,
    /// Run the gain-map math in the base image's color space rather than the
    /// alternate's.
    pub use_base_color_space: bool,
}

impl Default for GainMapMeta {
    /// SDR base, ~2 stops of headroom, gamma 1.0 — a sane starting point, not
    /// a spec default.
    fn default() -> Self {
        const EPS: f32 = 1.0 / 64.0;
        Self {
            min_log2: [0.0; 3],
            max_log2: [2.0; 3],
            gamma: [1.0; 3],
            base_offset: [EPS; 3],
            alt_offset: [EPS; 3],
            base_headroom: 0.0,
            alt_headroom: 2.0,
            use_base_color_space: true,
        }
    }
}
