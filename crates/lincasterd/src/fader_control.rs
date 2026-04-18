use lincaster_proto::BusState;
use tracing::{debug, info};

/// Controller for per-bus fader operations (gain, mute, solo).
#[allow(dead_code)]
pub struct FaderController<'a> {
    states: &'a mut Vec<BusState>,
}

#[allow(dead_code)]
impl<'a> FaderController<'a> {
    pub fn new(states: &'a mut Vec<BusState>) -> Self {
        info!("FaderController initialized with {} busses", states.len());
        Self { states }
    }

    /// Set the gain for a bus. Gain must be in [0.0, 1.0].
    pub fn set_gain(&mut self, bus_id: &str, gain: f32) -> Result<(), String> {
        if !(0.0..=1.0).contains(&gain) {
            return Err(format!("Gain {} out of range [0.0, 1.0]", gain));
        }
        match self.find_bus_mut(bus_id) {
            Some(state) => {
                debug!("Set gain for '{}': {} -> {}", bus_id, state.gain, gain);
                state.gain = gain;
                Ok(())
            }
            None => Err(format!("Bus '{}' not found", bus_id)),
        }
    }

    /// Get the current gain for a bus.
    pub fn get_gain(&self, bus_id: &str) -> Option<f32> {
        self.find_bus(bus_id).map(|s| s.gain)
    }

    /// Toggle mute for a bus.
    pub fn set_mute(&mut self, bus_id: &str, mute: bool) -> Result<(), String> {
        match self.find_bus_mut(bus_id) {
            Some(state) => {
                debug!("Set mute for '{}': {} -> {}", bus_id, state.mute, mute);
                state.mute = mute;
                Ok(())
            }
            None => Err(format!("Bus '{}' not found", bus_id)),
        }
    }

    /// Get the current mute state for a bus.
    pub fn is_muted(&self, bus_id: &str) -> Option<bool> {
        self.find_bus(bus_id).map(|s| s.mute)
    }

    /// Toggle solo for a bus. When a bus is soloed, all non-solo, non-solo-safe
    /// busses in the same group should be muted.
    pub fn set_solo(&mut self, bus_id: &str, solo: bool) -> Result<(), String> {
        // First verify the bus exists
        if self.find_bus(bus_id).is_none() {
            return Err(format!("Bus '{}' not found", bus_id));
        }

        // Set the solo state
        if let Some(state) = self.find_bus_mut(bus_id) {
            debug!("Set solo for '{}': {} -> {}", bus_id, state.solo, solo);
            state.solo = solo;
        }

        // Apply solo semantics: if any bus is soloed, mute all non-soloed busses
        self.apply_solo_logic();

        Ok(())
    }

    /// Check if any bus is currently in solo mode.
    pub fn any_solo_active(&self) -> bool {
        self.states.iter().any(|s| s.solo)
    }

    /// Apply solo muting logic. Called whenever solo state changes.
    fn apply_solo_logic(&mut self) {
        let any_soloed = self.states.iter().any(|s| s.solo);
        if !any_soloed {
            debug!("No solo active; solo muting cleared");
            return;
        }

        for state in self.states.iter() {
            if state.solo {
                debug!("  Solo active: '{}'", state.bus_id);
            } else if state.solo_safe {
                debug!("  Solo-safe (not muted): '{}'", state.bus_id);
            } else {
                debug!("  Solo-muted: '{}'", state.bus_id);
            }
        }
    }

    /// Set the solo_safe flag for a bus.
    pub fn set_solo_safe(&mut self, bus_id: &str, safe: bool) -> Result<(), String> {
        match self.find_bus_mut(bus_id) {
            Some(state) => {
                debug!(
                    "Set solo_safe for '{}': {} -> {}",
                    bus_id, state.solo_safe, safe
                );
                state.solo_safe = safe;
                Ok(())
            }
            None => Err(format!("Bus '{}' not found", bus_id)),
        }
    }

    /// Get the effective volume for a bus (considering gain, mute, solo, and solo_safe).
    /// Returns 0.0 if muted or solo-muted, otherwise the gain value.
    pub fn effective_volume(&self, bus_id: &str) -> f32 {
        let state = match self.find_bus(bus_id) {
            Some(s) => s,
            None => return 0.0,
        };

        // Explicitly muted always takes precedence
        if state.mute {
            return 0.0;
        }

        // Solo logic: if any bus is soloed and this bus is NOT soloed and NOT solo_safe, it's muted
        let any_soloed = self.any_solo_active();
        if any_soloed && !state.solo && !state.solo_safe {
            return 0.0;
        }

        state.gain
    }

    /// Check whether a bus is effectively muted by solo (but not by explicit mute).
    pub fn is_solo_muted(&self, bus_id: &str) -> bool {
        let state = match self.find_bus(bus_id) {
            Some(s) => s,
            None => return false,
        };
        let any_soloed = self.any_solo_active();
        any_soloed && !state.solo && !state.solo_safe
    }

    /// Get a snapshot of all bus states.
    pub fn all_states(&self) -> &[BusState] {
        self.states
    }

    fn find_bus(&self, bus_id: &str) -> Option<&BusState> {
        self.states.iter().find(|s| s.bus_id == bus_id)
    }

    fn find_bus_mut(&mut self, bus_id: &str) -> Option<&mut BusState> {
        self.states.iter_mut().find(|s| s.bus_id == bus_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_states() -> Vec<BusState> {
        vec![
            BusState {
                bus_id: "system".into(),
                gain: 1.0,
                mute: false,
                solo: false,
                solo_safe: true, // system is solo-safe by default
            },
            BusState {
                bus_id: "chat".into(),
                gain: 1.0,
                mute: false,
                solo: false,
                solo_safe: false,
            },
            BusState {
                bus_id: "game".into(),
                gain: 0.8,
                mute: false,
                solo: false,
                solo_safe: false,
            },
            BusState {
                bus_id: "music".into(),
                gain: 0.7,
                mute: false,
                solo: false,
                solo_safe: false,
            },
        ]
    }

    #[test]
    fn test_set_gain_valid() {
        let mut states = make_states();
        let mut ctrl = FaderController::new(&mut states);
        assert!(ctrl.set_gain("system", 0.5).is_ok());
        assert_eq!(ctrl.get_gain("system"), Some(0.5));
    }

    #[test]
    fn test_set_gain_invalid_range() {
        let mut states = make_states();
        let mut ctrl = FaderController::new(&mut states);
        assert!(ctrl.set_gain("system", 1.5).is_err());
        assert!(ctrl.set_gain("system", -0.1).is_err());
    }

    #[test]
    fn test_set_gain_unknown_bus() {
        let mut states = make_states();
        let mut ctrl = FaderController::new(&mut states);
        assert!(ctrl.set_gain("nonexistent", 0.5).is_err());
    }

    #[test]
    fn test_mute() {
        let mut states = make_states();
        let mut ctrl = FaderController::new(&mut states);
        assert_eq!(ctrl.effective_volume("system"), 1.0);
        ctrl.set_mute("system", true).unwrap();
        assert_eq!(ctrl.is_muted("system"), Some(true));
        assert_eq!(ctrl.effective_volume("system"), 0.0);
        ctrl.set_mute("system", false).unwrap();
        assert_eq!(ctrl.effective_volume("system"), 1.0);
    }

    #[test]
    fn test_solo() {
        let mut states = make_states();
        let mut ctrl = FaderController::new(&mut states);

        // Solo "chat" — chat audible, system audible (solo-safe), others muted
        ctrl.set_solo("chat", true).unwrap();
        assert_eq!(ctrl.effective_volume("chat"), 1.0);
        assert_eq!(ctrl.effective_volume("system"), 1.0); // solo-safe
        assert_eq!(ctrl.effective_volume("game"), 0.0);
        assert_eq!(ctrl.effective_volume("music"), 0.0);

        // Unsolo "chat" — everything audible again
        ctrl.set_solo("chat", false).unwrap();
        assert_eq!(ctrl.effective_volume("system"), 1.0);
        assert_eq!(ctrl.effective_volume("game"), 0.8);
    }

    #[test]
    fn test_solo_multiple() {
        let mut states = make_states();
        let mut ctrl = FaderController::new(&mut states);

        // Solo both "chat" and "game"
        ctrl.set_solo("chat", true).unwrap();
        ctrl.set_solo("game", true).unwrap();
        assert_eq!(ctrl.effective_volume("chat"), 1.0);
        assert_eq!(ctrl.effective_volume("game"), 0.8);
        assert_eq!(ctrl.effective_volume("system"), 1.0); // solo-safe
        assert_eq!(ctrl.effective_volume("music"), 0.0);
    }

    #[test]
    fn test_mute_overrides_solo() {
        let mut states = make_states();
        let mut ctrl = FaderController::new(&mut states);

        ctrl.set_solo("chat", true).unwrap();
        ctrl.set_mute("chat", true).unwrap();
        // Mute takes precedence over solo
        assert_eq!(ctrl.effective_volume("chat"), 0.0);
    }

    #[test]
    fn test_solo_safe() {
        let mut states = make_states();
        let mut ctrl = FaderController::new(&mut states);

        // System is solo-safe, so it shouldn't be muted when game is soloed
        ctrl.set_solo("game", true).unwrap();
        assert_eq!(ctrl.effective_volume("game"), 0.8);
        assert_eq!(ctrl.effective_volume("system"), 1.0); // solo-safe!
        assert_eq!(ctrl.effective_volume("chat"), 0.0); // not solo-safe
        assert_eq!(ctrl.effective_volume("music"), 0.0); // not solo-safe
        assert!(ctrl.is_solo_muted("chat"));
        assert!(!ctrl.is_solo_muted("system"));
        assert!(!ctrl.is_solo_muted("game"));
    }

    #[test]
    fn test_set_solo_safe() {
        let mut states = make_states();
        let mut ctrl = FaderController::new(&mut states);

        // Make chat solo-safe
        ctrl.set_solo_safe("chat", true).unwrap();
        // Solo game — both system (default solo-safe) and chat (newly solo-safe) should be audible
        ctrl.set_solo("game", true).unwrap();
        assert_eq!(ctrl.effective_volume("system"), 1.0);
        assert_eq!(ctrl.effective_volume("chat"), 1.0);
        assert_eq!(ctrl.effective_volume("game"), 0.8);
        assert_eq!(ctrl.effective_volume("music"), 0.0);
    }
}
