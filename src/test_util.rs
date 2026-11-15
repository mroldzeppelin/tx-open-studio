//! Test helpers shared across modules.

use crate::*;

/// An AppWindow on Slint's software renderer (no display needed). The
/// platform is process-wide: set once, ignored after.
pub(crate) fn headless_ui() -> AppWindow {
    use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
    use slint::platform::{Platform, WindowAdapter};
    use std::rc::Rc;
    struct Headless;
    impl Platform for Headless {
        fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, slint::PlatformError> {
            Ok(MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer))
        }
    }
    let _ = slint::platform::set_platform(Box::new(Headless));
    AppWindow::new().unwrap()
}
