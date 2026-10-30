//! The tray and window icon, drawn rather than shipped as a file.

pub const SIZE: u32 = 32;

/// A ring whose filled arc is `fraction` of the way round — the memory gauge
/// — on a transparent background. Green when comfortable, amber, then red.
pub fn gauge(fraction: f32) -> Vec<u8> {
    let f = fraction.clamp(0.0, 1.0);
    let colour: [u8; 3] = if f < 0.6 {
        [74, 222, 128]
    } else if f < 0.85 {
        [250, 204, 21]
    } else {
        [248, 113, 113]
    };
    let mut rgba = vec![0u8; (SIZE * SIZE * 4) as usize];
    let c = (SIZE as f32 - 1.0) / 2.0;
    for y in 0..SIZE {
        for x in 0..SIZE {
            let (dx, dy) = (x as f32 - c, y as f32 - c);
            let r = (dx * dx + dy * dy).sqrt();
            if !(9.0..=15.0).contains(&r) {
                continue;
            }
            // Angle clockwise from twelve o'clock, 0..1.
            let a = (dx.atan2(-dy) / std::f32::consts::TAU).rem_euclid(1.0);
            let i = ((y * SIZE + x) * 4) as usize;
            let (rgb, alpha) = if a <= f { (colour, 255) } else { ([90, 90, 100], 200) };
            rgba[i..i + 3].copy_from_slice(&rgb);
            rgba[i + 3] = alpha;
        }
    }
    rgba
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_gauge_is_a_full_rgba_image() {
        let img = super::gauge(0.31);
        assert_eq!(img.len(), (super::SIZE * super::SIZE * 4) as usize);
        assert!(img.chunks(4).any(|p| p[3] == 255), "the filled arc is drawn");
    }
}
