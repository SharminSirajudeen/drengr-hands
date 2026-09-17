pub mod annotate;
pub mod optimize;
pub mod registry;
pub mod text_scene;
pub mod ui_element;

pub use annotate::ScreenAnnotator;
pub use optimize::ImageOptimizer;
pub use registry::ElementRegistry;
pub use text_scene::TextSceneBuilder;
pub use ui_element::{Bounds, DeviceInfo, Point, UiElement};
