/// Trait for external sync sources (Link, MIDI Clock, OSC).
/// No implementations — trait definition only for Phase 7.
pub trait SyncSource: Send {
    fn is_enabled(&self) -> bool;
    fn display_name(&self) -> &str;
    fn enable(&mut self);
    fn disable(&mut self);

    fn toggle(&mut self) {
        if self.is_enabled() {
            self.disable();
        } else {
            self.enable();
        }
    }
}
