//! Display geometry helpers. macOS uses points for bounds; Windows uses physical pixels.

use crate::DisplayInfo;

/// Build [`DisplayInfo`] from a display's bounds in **points** (macOS / CG coordinate space).
/// `scale` is `pixels_wide / width_pts` (Retina metadata only; not applied to x/y/width/height).
pub fn display_info_from_points(
    id: String,
    name: String,
    origin_x: f64,
    origin_y: f64,
    width_pts: f64,
    height_pts: f64,
    pixels_wide: u32,
    pixels_high: u32,
    primary: bool,
) -> DisplayInfo {
    let scale = if width_pts > 0.5 && height_pts > 0.5 {
        let scale_x = pixels_wide as f64 / width_pts;
        let scale_y = pixels_high as f64 / height_pts;
        (scale_x + scale_y) / 2.0
    } else if width_pts > 0.5 {
        pixels_wide as f64 / width_pts
    } else {
        1.0
    };
    DisplayInfo {
        id,
        name,
        x: origin_x.round() as i32,
        y: origin_y.round() as i32,
        width: width_pts.round() as u32,
        height: height_pts.round() as u32,
        scale,
        primary,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retina_2x_reports_points_and_scale() {
        let d = display_info_from_points(
            "1".into(),
            "Built-in".into(),
            0.0,
            0.0,
            1440.0,
            900.0,
            2880,
            1800,
            true,
        );
        assert_eq!(d.width, 1440);
        assert_eq!(d.height, 900);
        assert!((d.scale - 2.0).abs() < 0.01);
    }

    #[test]
    fn non_retina_1x() {
        let d = display_info_from_points(
            "2".into(),
            "Ext".into(),
            1920.0,
            0.0,
            1920.0,
            1080.0,
            1920,
            1080,
            false,
        );
        assert_eq!(d.width, 1920);
        assert_eq!(d.x, 1920);
        assert!((d.scale - 1.0).abs() < 0.01);
    }

    #[test]
    fn scaled_mode_fractional_points() {
        let d = display_info_from_points(
            "3".into(),
            "Laptop".into(),
            0.0,
            0.0,
            1512.0,
            982.0,
            3024,
            1964,
            true,
        );
        assert_eq!(d.width, 1512);
        assert_eq!(d.height, 982);
        assert!((d.scale - 2.0).abs() < 0.01);
    }

    #[test]
    fn negative_origin_in_points() {
        let d = display_info_from_points(
            "4".into(),
            "Left".into(),
            -1920.0,
            100.0,
            1920.0,
            1080.0,
            1920,
            1080,
            false,
        );
        assert_eq!(d.x, -1920);
        assert_eq!(d.y, 100);
    }
}
