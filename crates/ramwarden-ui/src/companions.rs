//! Pixel geometry and expressions matching the Mac companions.
//! Native renderers consume this module; no platform-specific redraws.
use crate::wardens::Sprite;

pub const RAM_CELLS: &str = include_str!("../../../assets/companions/ram.cells");
pub const THREAT_SVG: &str = include_str!("../../../web/assets/pets/threat.svg");

#[derive(Clone, Copy)]
pub enum Pupil {
    Slit,
    Round,
    None,
}
#[derive(Clone)]
pub struct Appearance {
    pub wool: [u8; 3],
    pub horns: [u8; 3],
    pub hooves: [u8; 3],
    pub eyes: [u8; 3],
    pub pupil: Pupil,
}
impl Appearance {
    pub fn ram() -> Self {
        Self {
            wool: [0xd8, 0x45, 0x45],
            horns: [0xf5, 0xde, 0xaa],
            hooves: [0x29, 0x24, 0x2b],
            eyes: [255; 3],
            pupil: Pupil::Slit,
        }
    }
    pub fn threat() -> Self {
        Self {
            wool: [0x9a, 0xa3, 0xad],
            horns: [0x7c, 0x3a, 0xed],
            hooves: [0x1a, 0x0f, 0x2e],
            eyes: [0x39, 255, 0x88],
            pupil: Pupil::None,
        }
    }
}
fn tint(c: [u8; 3], factor: f64) -> [u8; 3] {
    c.map(|v| (f64::from(v) * factor).min(255.).round() as u8)
}
fn shade(c: [u8; 3], amount: f64) -> [u8; 3] {
    c.map(|v| {
        let v = f64::from(v);
        (if amount >= 0. {
            v + (255. - v) * amount
        } else {
            v * (1. + amount)
        })
        .round() as u8
    })
}
fn color(k: u8, a: &Appearance) -> [u8; 3] {
    match k {
        b'w' => a.wool,
        b's' => tint(a.wool, 0.62),
        b'b' => tint(a.wool, 1.3),
        b'h' => a.horns,
        b'H' => tint(a.horns, 1.25),
        b'j' => tint(a.horns, 0.66),
        b'k' => a.hooves,
        b'e' => a.eyes,
        b'E' => tint(a.eyes, 0.76),
        _ => [26, 13, 20],
    }
}
pub fn ram(frame: u64, thinking: bool, speaking: bool, a: &Appearance) -> Sprite {
    let mut cells: Vec<u8> = RAM_CELLS
        .bytes()
        .filter(|c| !c.is_ascii_whitespace())
        .collect();
    let mut rect = |x: usize, y: usize, w: usize, h: usize, k: u8| {
        for yy in y..y + h {
            for xx in x..x + w {
                cells[yy * 48 + xx] = k;
            }
        }
    };
    for x in [15, 26] {
        rect(x - 1, 19, 9, 6, b'o');
        if frame.is_multiple_of(25) && !thinking {
            rect(x, 22, 7, 1, b'E');
        } else {
            rect(x, 20, 7, 4, b'e');
            rect(x + 1, 23, 5, 1, b'E');
            match a.pupil {
                Pupil::Slit => rect(x + 1, 21, 5, 1, b'o'),
                Pupil::Round => {
                    rect(x + 3, 21, 2, 2, b'o');
                    rect(x + 3, 20, 1, 1, b'e');
                }
                Pupil::None => {}
            }
        }
    }
    if speaking && frame.is_multiple_of(2) {
        rect(23, 34, 3, 2, b'o');
    } else {
        rect(23, 35, 3, 1, b'o');
    }
    let rgba = cells
        .into_iter()
        .flat_map(|k| {
            if k == b'.' {
                [0; 4]
            } else {
                let [r, g, b] = color(k, a);
                [r, g, b, 255]
            }
        })
        .collect();
    Sprite {
        width: 48,
        height: 48,
        rgba,
    }
}
pub fn ram_bob(frame: u64, thinking: bool) -> i32 {
    if thinking {
        if frame.is_multiple_of(2) { 0 } else { 2 }
    } else if frame % 8 < 4 {
        0
    } else {
        1
    }
}
pub fn threat(a: &Appearance) -> Sprite {
    let mut s = Sprite::from_svg(THREAT_SVG).expect("bundled Mac ThreatWarden sprite");
    for (i, p) in s.rgba.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        if p[3] == 0 {
            continue;
        }
        let c = [p[0], p[1], p[2]];
        let c = if i / 48 >= 43 && c != [7, 5, 11] {
            shade(a.hooves, if c == [227, 232, 238] { 0.25 } else { 0. })
        } else {
            match c {
                [154, 163, 173] => a.wool,
                [227, 232, 238] => shade(a.wool, 0.65),
                [91, 99, 109] => shade(a.wool, -0.4),
                [91, 33, 182] | [124, 58, 237] => a.horns,
                [139, 92, 246] => shade(a.horns, 0.18),
                [221, 214, 254] | [196, 181, 253] => shade(a.horns, 0.65),
                [59, 15, 122] | [26, 15, 46] => shade(a.horns, -0.5),
                [57, 255, 136] => a.eyes,
                [217, 255, 233] | [234, 255, 242] | [167, 243, 199] => shade(a.eyes, 0.65),
                _ => c,
            }
        };
        p[..3].copy_from_slice(&c);
    }
    s
}
pub fn panel_size(threat: bool, scale: u32) -> [i32; 2] {
    let px = 48 * scale.clamp(2, 6) as i32;
    [px + if threat { 16 } else { 36 }, px + 36]
}
#[cfg(test)]
mod tests {
    use super::*;
    // Golden RGBA checksums from the pre-migration Mac PixelRam geometry and
    // palette, independently rasterized from the original cell-for-cell port.
    #[test]
    fn mac_default_frames_are_pixel_exact() {
        fn hash(s: Sprite) -> u64 {
            s.rgba.into_iter().fold(14695981039346656037, |h, b| {
                (h ^ u64::from(b)).wrapping_mul(1099511628211)
            })
        }
        let a = Appearance::ram();
        assert_eq!(hash(ram(1, false, false, &a)), 13056338334306304345);
        assert_eq!(hash(ram(0, false, false, &a)), 8555682707904083609);
        assert_eq!(hash(ram(2, false, true, &a)), 16204615545737139116);
    }
    #[test]
    fn geometry_and_mac_timing() {
        assert_eq!(RAM_CELLS.lines().count(), 48);
        assert!(RAM_CELLS.lines().all(|l| l.len() == 48));
        let a = Appearance::ram();
        assert_ne!(ram(0, false, false, &a), ram(1, false, false, &a));
        assert_eq!(ram(0, true, false, &a), ram(1, true, false, &a));
        assert_ne!(ram(2, false, true, &a), ram(3, false, true, &a));
        assert_eq!(
            (0..8).map(|f| ram_bob(f, false)).collect::<Vec<_>>(),
            [0, 0, 0, 0, 1, 1, 1, 1]
        );
        assert_eq!(panel_size(false, 3), [180, 180]);
        assert_eq!(panel_size(true, 3), [160, 180]);
    }
    #[test]
    fn threat_uses_mac_asset_and_palette() {
        let s = threat(&Appearance::threat());
        assert_eq!(s.width, 48);
        assert!(s.rgba.as_chunks::<4>().0.contains(&[124, 58, 237, 255]));
        assert_ne!(
            THREAT_SVG,
            include_str!("../../../web/assets/wardens/threat.svg")
        );
    }
}
