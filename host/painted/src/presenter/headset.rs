//! What the window loop asks of a Windows headset session.
use super::Presenter;
use exact_runner::DataSource;

impl<D: DataSource> Presenter<D> {
    /// The headset's frame (`surfaces/headset.rs`, "PC VR on Windows"):
    /// while a session exists, the primary module's canvas draws one frame
    /// in the headset (which paces the call) and what it published and
    /// posted reaches the page, whether or not the window shows. Some(whether
    /// the canvas wants another frame), or None when no session draws.
    pub fn headset_frame(&mut self) -> Option<bool> {
        if !crate::surfaces::headset::drawing() {
            return None;
        }
        let id = self.surfaces.headset_canvas()?;
        let code = crate::surfaces::headset::render(id, self.host.now());
        if !crate::surfaces::headset::drawing() {
            self.surfaces.restore_desktop_canvas_lifecycle();
            self.repaint();
        }
        self.sync_surfaces();
        code.map(|code| code == 1)
    }

    /// Whether the runtime currently gives this game headset input.
    pub fn headset_focused(&self) -> bool {
        self.surfaces.headset_canvas().is_some() && crate::surfaces::headset::state() == 3
    }
}
