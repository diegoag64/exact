use super::*;

impl<G: Game> Sim<G> {
    /// Queue a headset's head and controllers at the current clock, as a
    /// headset host reports them each frame (`Input::xr`).
    pub fn xr(&mut self, input: crate::XrInput) {
        self.input(InputEvent::Xr {
            input,
            at_ms: self.last_us.unwrap_or(0) as f64 / 1000.0,
            player: 0,
        });
    }
}
