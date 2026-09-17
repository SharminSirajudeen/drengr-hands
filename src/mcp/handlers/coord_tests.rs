use super::norm_to_px;
#[test]
fn norm_to_px_maps_and_clamps() {
    assert_eq!(norm_to_px(0.0, 0.0, 402, 874), (0, 0));
    assert_eq!(norm_to_px(1.0, 1.0, 402, 874), (402, 874));
    assert_eq!(norm_to_px(0.5, 0.5, 400, 800), (200, 400));
    // out-of-range estimates are clamped on-screen
    assert_eq!(norm_to_px(-0.3, 1.7, 402, 874), (0, 874));
}
