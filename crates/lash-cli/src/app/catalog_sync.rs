use super::App;

#[derive(Default)]
pub(super) struct AutomaticToolCatalogSyncState {
    disabled: bool,
}

impl AutomaticToolCatalogSyncState {
    fn should_attempt(&self) -> bool {
        !self.disabled
    }

    fn observe_result<E>(&mut self, result: &Result<(), E>) {
        if result.is_err() {
            self.disabled = true;
        }
    }

    fn reset(&mut self) {
        self.disabled = false;
    }
}

impl App {
    pub(crate) fn should_automatically_sync_tool_catalog(&self) -> bool {
        self.automatic_tool_catalog_sync.should_attempt()
    }

    pub(crate) fn observe_automatic_tool_catalog_sync_result<E>(&mut self, result: &Result<(), E>) {
        self.automatic_tool_catalog_sync.observe_result(result);
    }

    pub(crate) fn reset_automatic_tool_catalog_sync(&mut self) {
        self.automatic_tool_catalog_sync.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::AutomaticToolCatalogSyncState;

    #[test]
    fn failure_disables_automatic_sync() {
        let mut state = AutomaticToolCatalogSyncState::default();

        state.observe_result(&Err::<(), _>("sync failed"));

        assert!(!state.should_attempt());
    }

    #[test]
    fn new_session_resets_automatic_sync() {
        let mut state = AutomaticToolCatalogSyncState::default();
        state.observe_result(&Err::<(), _>("sync failed"));

        state.reset();

        assert!(state.should_attempt());
    }

    #[test]
    fn successful_sync_keeps_automatic_sync_enabled() {
        let mut state = AutomaticToolCatalogSyncState::default();

        state.observe_result(&Ok::<_, &str>(()));

        assert!(state.should_attempt());
    }
}
