use super::*;

#[test]
fn embedded_app_icon_decodes_to_square_rgba_with_transparency() {
    let icon =
        eframe::icon_data::from_png_bytes(include_bytes!("../../assets/app-icon.png")).unwrap();
    assert_eq!((icon.width, icon.height), (1024, 1024));
    assert_eq!(icon.rgba.len(), (icon.width * icon.height * 4) as usize);
    assert!(
        icon.rgba
            .as_chunks::<4>()
            .0
            .iter()
            .any(|pixel| pixel[3] == 0)
    );
    assert!(
        icon.rgba
            .as_chunks::<4>()
            .0
            .iter()
            .any(|pixel| pixel[3] == 255)
    );
}
