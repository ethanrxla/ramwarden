//! The approved white-eyed RamWarden mark. RAM usage remains in the tray tooltip.
pub const SIZE: u32 = 32;
pub fn gauge(_fraction: f32) -> Vec<u8> {
    include_bytes!("../../../../assets/brand/icon32.rgba").to_vec()
}
#[cfg(test)]
mod tests {
    #[test]
    fn brand_icon_has_complete_rgba_pixels() {
        let image=super::gauge(0.5);
        assert_eq!(image.len(),(super::SIZE*super::SIZE*4) as usize);
        assert!(image.as_chunks::<4>().0.iter().any(|pixel| pixel[3] > 0));
    }
}
