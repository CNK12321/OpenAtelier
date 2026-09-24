//! How a file's pixel values are read as light (DESIGN.md §9).
//!
//! The working space is scene-linear Rec.709 primaries, where 1.0 is SDR reference
//! white (203 nits for HDR sources, per BT.2408) and log footage puts 18% gray at 0.18.
//! Every file gets an **input transform**: its YCbCr matrix and range (used when the
//! decoder turns YCbCr into RGB), then its transfer curve, gamut and an exposure trim.
//! Tags in the file choose the defaults, but the user can override every part — file
//! metadata is often missing or wrong (phones tag log footage as Rec.709).

use serde::{Deserialize, Serialize};

/// The curve a file's values were encoded with.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Transfer {
    /// From the file's tags: PQ or HLG when tagged so, otherwise sRGB.
    #[default]
    Auto,
    /// The sRGB curve (also what untagged SDR video is decoded with, so an unedited clip
    /// shows exactly as it would in a player on an sRGB display).
    Srgb,
    /// Rec.709 on a BT.1886 display: a 2.4 gamma.
    Rec709,
    Gamma22,
    Gamma26,
    Linear,
    /// HDR10 / Dolby Vision base layer (SMPTE ST 2084).
    Pq,
    /// Hybrid log-gamma (broadcast and phone HDR), with a 1000-nit display's system gamma.
    Hlg,
    /// ARRI LogC3 (EI 800).
    LogC3,
    /// Sony S-Log3.
    SLog3,
    /// Panasonic V-Log.
    VLog,
    /// Fujifilm F-Log.
    FLog,
    /// Canon Log 3.
    CanonLog3,
    /// Apple Log (iPhone 15 Pro and later).
    AppleLog,
}

impl Transfer {
    pub const ALL: [Transfer; 14] = [
        Transfer::Auto,
        Transfer::Srgb,
        Transfer::Rec709,
        Transfer::Gamma22,
        Transfer::Gamma26,
        Transfer::Linear,
        Transfer::Pq,
        Transfer::Hlg,
        Transfer::LogC3,
        Transfer::SLog3,
        Transfer::VLog,
        Transfer::FLog,
        Transfer::CanonLog3,
        Transfer::AppleLog,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Transfer::Auto => "Automatic",
            Transfer::Srgb => "sRGB",
            Transfer::Rec709 => "Rec.709 (gamma 2.4)",
            Transfer::Gamma22 => "Gamma 2.2",
            Transfer::Gamma26 => "Gamma 2.6",
            Transfer::Linear => "Linear",
            Transfer::Pq => "HDR PQ (HDR10)",
            Transfer::Hlg => "HDR HLG",
            Transfer::LogC3 => "ARRI LogC3",
            Transfer::SLog3 => "Sony S-Log3",
            Transfer::VLog => "Panasonic V-Log",
            Transfer::FLog => "Fujifilm F-Log",
            Transfer::CanonLog3 => "Canon Log 3",
            Transfer::AppleLog => "Apple Log",
        }
    }

    /// Scene values can go far past 1.0: HDR or log footage, whose highlights need tone
    /// mapping to look right on an SDR screen.
    pub fn is_high_range(self) -> bool {
        !matches!(self, Transfer::Auto | Transfer::Srgb | Transfer::Rec709 | Transfer::Gamma22 | Transfer::Gamma26 | Transfer::Linear)
    }

    /// Index the input shader switches on (`oa.internal.input`).
    pub fn shader_index(self) -> u32 {
        match self {
            Transfer::Auto | Transfer::Srgb => 0,
            Transfer::Rec709 => 1,
            Transfer::Gamma22 => 2,
            Transfer::Gamma26 => 3,
            Transfer::Linear => 4,
            Transfer::Pq => 5,
            Transfer::Hlg => 6,
            Transfer::LogC3 => 7,
            Transfer::SLog3 => 8,
            Transfer::VLog => 9,
            Transfer::FLog => 10,
            Transfer::CanonLog3 => 11,
            Transfer::AppleLog => 12,
        }
    }

    /// The camera's usual gamut for a log curve.
    pub fn native_gamut(self) -> Gamut {
        match self {
            Transfer::Pq | Transfer::Hlg | Transfer::FLog | Transfer::AppleLog => Gamut::Rec2020,
            Transfer::LogC3 => Gamut::ArriWideGamut3,
            Transfer::SLog3 => Gamut::SGamut3Cine,
            Transfer::VLog => Gamut::VGamut,
            Transfer::CanonLog3 => Gamut::CinemaGamut,
            _ => Gamut::Rec709,
        }
    }
}

/// The primaries a file's RGB values refer to (all D65).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Gamut {
    /// From the file's tags, else the curve's usual camera gamut, else Rec.709.
    #[default]
    Auto,
    Rec709,
    Rec2020,
    DisplayP3,
    ArriWideGamut3,
    SGamut3,
    SGamut3Cine,
    VGamut,
    CinemaGamut,
}

impl Gamut {
    pub const ALL: [Gamut; 9] = [
        Gamut::Auto,
        Gamut::Rec709,
        Gamut::Rec2020,
        Gamut::DisplayP3,
        Gamut::ArriWideGamut3,
        Gamut::SGamut3,
        Gamut::SGamut3Cine,
        Gamut::VGamut,
        Gamut::CinemaGamut,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Gamut::Auto => "Automatic",
            Gamut::Rec709 => "Rec.709 / sRGB",
            Gamut::Rec2020 => "Rec.2020",
            Gamut::DisplayP3 => "Display P3",
            Gamut::ArriWideGamut3 => "ARRI Wide Gamut 3",
            Gamut::SGamut3 => "Sony S-Gamut3",
            Gamut::SGamut3Cine => "Sony S-Gamut3.Cine",
            Gamut::VGamut => "Panasonic V-Gamut",
            Gamut::CinemaGamut => "Canon Cinema Gamut",
        }
    }

    /// Red, green and blue primaries as CIE xy.
    fn primaries(self) -> [[f64; 2]; 3] {
        match self {
            Gamut::Auto | Gamut::Rec709 => [[0.64, 0.33], [0.30, 0.60], [0.15, 0.06]],
            Gamut::Rec2020 => [[0.708, 0.292], [0.170, 0.797], [0.131, 0.046]],
            Gamut::DisplayP3 => [[0.680, 0.320], [0.265, 0.690], [0.150, 0.060]],
            Gamut::ArriWideGamut3 => [[0.6840, 0.3130], [0.2210, 0.8480], [0.0861, -0.1020]],
            Gamut::SGamut3 => [[0.730, 0.280], [0.140, 0.855], [0.100, -0.050]],
            Gamut::SGamut3Cine => [[0.766, 0.275], [0.225, 0.800], [0.089, -0.087]],
            Gamut::VGamut => [[0.730, 0.280], [0.165, 0.840], [0.100, -0.030]],
            Gamut::CinemaGamut => [[0.740, 0.270], [0.170, 1.140], [0.080, -0.100]],
        }
    }

    /// Row-major matrix taking linear RGB in this gamut to linear Rec.709.
    pub fn to_rec709(self) -> [[f64; 3]; 3] {
        let from = rgb_to_xyz(self.primaries());
        let to = invert(rgb_to_xyz(Gamut::Rec709.primaries()));
        mul(to, from)
    }
}

/// YCbCr → RGB coefficients override (the decoder's own choice when `Auto`).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Matrix {
    #[default]
    Auto,
    Bt601,
    Bt709,
    Bt2020,
}

/// Whether YCbCr uses the whole code range or the 16–235 "video" range.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Range {
    #[default]
    Auto,
    Limited,
    Full,
}

/// A file's input transform. The default reads the file's own tags.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct InputColor {
    pub transfer: Transfer,
    pub gamut: Gamut,
    pub matrix: Matrix,
    pub range: Range,
    /// Exposure trim in stops, applied in linear light.
    pub exposure: f64,
}

impl InputColor {
    pub fn is_auto(&self) -> bool {
        *self == InputColor::default()
    }

    /// The transfer and gamut actually used, given the file's tags.
    pub fn resolve(&self, tags: &ColorTags) -> (Transfer, Gamut) {
        let transfer = match self.transfer {
            Transfer::Auto => match tags.transfer.as_deref() {
                Some("smpte2084") => Transfer::Pq,
                Some("arib-std-b67") => Transfer::Hlg,
                Some("linear") => Transfer::Linear,
                _ => Transfer::Srgb,
            },
            t => t,
        };
        let gamut = match self.gamut {
            Gamut::Auto => match tags.primaries.as_deref() {
                Some("bt2020") => Gamut::Rec2020,
                Some("smpte432") => Gamut::DisplayP3,
                Some("bt709" | "bt470bg" | "smpte170m") => Gamut::Rec709,
                _ => transfer.native_gamut(),
            },
            g => g,
        };
        (transfer, gamut)
    }
}

/// Color tags read from the file (ffprobe's names), kept to resolve `Auto`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ColorTags {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transfer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primaries: Option<String>,
}

impl ColorTags {
    pub fn is_empty(&self) -> bool {
        self.transfer.is_none() && self.primaries.is_none()
    }
}

/// How the working space is squeezed onto an SDR screen and into SDR files.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ToneMap {
    /// Clip at white (right for SDR material: nothing changes).
    Off,
    /// Leave everything below 80% alone and roll highlights off smoothly to white,
    /// keeping their hue.
    Soft,
    /// A film-like S-curve over the whole range (ACES-style), for log footage.
    Filmic,
}

impl ToneMap {
    pub fn shader_index(self) -> u32 {
        match self {
            ToneMap::Off => 0,
            ToneMap::Soft => 1,
            ToneMap::Filmic => 2,
        }
    }
}

fn rgb_to_xyz(p: [[f64; 2]; 3]) -> [[f64; 3]; 3] {
    const WHITE: [f64; 2] = [0.3127, 0.3290];
    let xyz = |[x, y]: [f64; 2]| [x / y, 1.0, (1.0 - x - y) / y];
    let (r, g, b) = (xyz(p[0]), xyz(p[1]), xyz(p[2]));
    let m = [[r[0], g[0], b[0]], [r[1], g[1], b[1]], [r[2], g[2], b[2]]];
    let s = apply(invert(m), xyz(WHITE));
    [[m[0][0] * s[0], m[0][1] * s[1], m[0][2] * s[2]], [m[1][0] * s[0], m[1][1] * s[1], m[1][2] * s[2]], [m[2][0] * s[0], m[2][1] * s[1], m[2][2] * s[2]]]
}

fn apply(m: [[f64; 3]; 3], v: [f64; 3]) -> [f64; 3] {
    [0, 1, 2].map(|r| m[r][0] * v[0] + m[r][1] * v[1] + m[r][2] * v[2])
}

fn mul(a: [[f64; 3]; 3], b: [[f64; 3]; 3]) -> [[f64; 3]; 3] {
    [0, 1, 2].map(|r| [0, 1, 2].map(|c| a[r][0] * b[0][c] + a[r][1] * b[1][c] + a[r][2] * b[2][c]))
}

fn invert(m: [[f64; 3]; 3]) -> [[f64; 3]; 3] {
    let [[a, b, c], [d, e, f], [g, h, i]] = m;
    let det = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g);
    let k = 1.0 / det;
    [
        [(e * i - f * h) * k, (c * h - b * i) * k, (b * f - c * e) * k],
        [(f * g - d * i) * k, (a * i - c * g) * k, (c * d - a * f) * k],
        [(d * h - e * g) * k, (b * g - a * h) * k, (a * e - b * d) * k],
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rec709_to_itself_is_identity() {
        let m = Gamut::Rec709.to_rec709();
        for (r, row) in m.iter().enumerate() {
            for (c, v) in row.iter().enumerate() {
                assert!((v - if r == c { 1.0 } else { 0.0 }).abs() < 1e-9, "{m:?}");
            }
        }
    }

    #[test]
    fn rec2020_matrix_matches_bt2087() {
        // BT.2087's published Rec.2020 → Rec.709 matrix (linear light).
        let want = [[1.6605, -0.5876, -0.0728], [-0.1246, 1.1329, -0.0083], [-0.0182, -0.1006, 1.1187]];
        let m = Gamut::Rec2020.to_rec709();
        for r in 0..3 {
            for c in 0..3 {
                assert!((m[r][c] - want[r][c]).abs() < 2e-3, "{m:?}");
            }
        }
        // White stays white in every gamut.
        for g in Gamut::ALL {
            let w = apply(g.to_rec709(), [1.0; 3]);
            assert!(w.iter().all(|x| (x - 1.0).abs() < 1e-6), "{g:?}: {w:?}");
        }
    }

    #[test]
    fn auto_follows_the_tags() {
        let pq = ColorTags { transfer: Some("smpte2084".into()), primaries: Some("bt2020".into()) };
        assert_eq!(InputColor::default().resolve(&pq), (Transfer::Pq, Gamut::Rec2020));
        assert_eq!(InputColor::default().resolve(&ColorTags::default()), (Transfer::Srgb, Gamut::Rec709));
        // A log curve picked by hand brings its camera's gamut along, unless tagged.
        let slog = InputColor { transfer: Transfer::SLog3, ..Default::default() };
        assert_eq!(slog.resolve(&ColorTags::default()), (Transfer::SLog3, Gamut::SGamut3Cine));
        assert!(Transfer::Pq.is_high_range() && !Transfer::Rec709.is_high_range());
    }
}
