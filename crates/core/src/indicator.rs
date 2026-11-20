//! Platform-independent rendering helpers for the desktop status indicators.

/// Turn the icon's amber pixels red without changing the dark logo or transparency.
pub fn recording_rgba(mut rgba: Vec<u8>) -> Vec<u8> {
    for pixel in rgba.chunks_exact_mut(4) {
        if pixel[0] > pixel[1] && pixel[1] > pixel[2] {
            pixel[1] = pixel[2];
        }
    }
    rgba
}

/// Place a popup within a monitor's physical work area, including negative monitor origins.
pub fn popup_position(origin: (i32, i32), area: (u32, u32), size: (u32, u32), centered: bool) -> (i32, i32) {
    let free_x = area.0.saturating_sub(size.0) as i32;
    let free_y = area.1.saturating_sub(size.1) as i32;
    let (x, y) = if centered { (free_x / 2, free_y / 2) } else { ((free_x - 16).max(0), (free_y - 16).max(0)) };
    (origin.0 + x, origin.1 + y)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recording_icon_is_red_and_preserves_logo_edges_and_alpha() {
        let idle = vec![255, 177, 32, 255, 128, 89, 16, 128, 9, 9, 9, 255, 0, 0, 0, 0];
        let recording = recording_rgba(idle.clone());
        assert_eq!(recording, [255, 32, 32, 255, 128, 16, 16, 128, 9, 9, 9, 255, 0, 0, 0, 0]);
        assert_eq!(&idle[..4], &[255, 177, 32, 255], "the idle icon stays amber");
    }

    #[test]
    fn processing_popup_is_centered_using_configured_physical_size() {
        assert_eq!(popup_position((0, 0), (1920, 1080), (84, 84), true), (918, 498));
        assert_eq!(popup_position((-1920, -100), (1920, 1080), (168, 168), true), (-1044, 356));
        assert_eq!(popup_position((0, 0), (1920, 1080), (84, 84), false), (1820, 980));
        assert_eq!(popup_position((10, 20), (60, 60), (84, 84), true), (10, 20));
        assert_eq!(popup_position((10, 20), (60, 60), (84, 84), false), (10, 20));
    }
}
