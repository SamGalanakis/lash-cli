use super::*;

impl App {
    pub fn clear(&mut self) {
        self.reset_automatic_tool_catalog_sync();
        self.timeline = Vec::new().into();
        self.scroll_offset = 0;
        self.follow_mode = FollowOutputMode::PinnedBottom;
        self.live.assistant.clear();
        self.live.reasoning.clear();
        self.clear_status();
        self.editor.pending_images.clear();
        self.editor.pending_large_pastes.clear();
        self.clear_live_tool_output();
        self.queues.draft_presentations.clear();
        self.clear_pending_turn_input_snapshot();
        self.overlay = None;
        self.activity_state.reset();
        self.ui_activity_journal = UiActivityJournal::default();
        self.pending_ui_activity_records.clear();
        self.active_ui_turn_ordinal = None;
        self.next_lashlang_block_ordinal = 0;
        self.lashlang_block_anchors.clear();
        self.usage.token_usage = TokenUsage::default();
        self.usage.last_response_usage = TokenUsage::default();
        self.usage.last_prompt_usage = None;
        self.usage.live_output_chars_estimate = 0;
        self.usage.live_output_tokens_estimate = 0;
        self.model_variant = None;
        self.clear_mode_indicators();
        self.plan_dock = None;
        self.processes.clear();
        self.selected_process_id = None;
        self.invalidate_height_cache();
    }

    pub fn restore_prepared_turn(&mut self, turn: PreparedTurn) {
        self.editor
            .restore_turn(turn.display_text, turn.images, turn.large_pastes);
        self.update_suggestions();
    }

    pub fn next_image_marker_id(&self) -> usize {
        self.editor.next_image_marker_id()
    }

    pub fn begin_pending_image(&mut self, id: usize) {
        self.editor.begin_pending_image(id);
    }

    pub fn has_pending_image_jobs(&self) -> bool {
        self.editor.has_pending_image_jobs()
    }

    pub fn complete_pending_image(&mut self, id: usize, png_bytes: Vec<u8>) -> bool {
        self.editor.complete_pending_image(id, png_bytes)
    }

    pub fn fail_pending_image(&mut self, id: usize) -> bool {
        self.editor.fail_pending_image(id)
    }

    pub fn cycle_expand(&mut self) {
        let new_level = if self.expand_level == 0 { 1 } else { 0 };
        self.set_expand_level(new_level);
    }

    pub fn toggle_full_expand(&mut self) {
        let new_level = if self.expand_level != 2 { 2 } else { 1 };
        self.set_expand_level(new_level);
    }

    pub fn set_expand_level(&mut self, level: u8) {
        if self.follows_output() {
            self.expand_level = level;
            self.invalidate_height_cache();
            if self.render_cache.width > 0 && self.render_cache.viewport_height > 0 {
                self.ensure_height_cache(
                    self.render_cache.width,
                    self.render_cache.viewport_height,
                );
                self.scroll_offset = self.follow_scroll_offset(
                    self.render_cache.width,
                    self.render_cache.viewport_height,
                );
            } else {
                self.scroll_offset = usize::MAX;
            }
            return;
        }

        // Manual scroll: the toggle still applies, but every offset below
        // the viewport moves when blocks change height, so re-anchor to the
        // block the reader was looking at instead of keeping a raw line
        // offset that now points somewhere else.
        let width = self.render_cache.width;
        let viewport_height = self.render_cache.viewport_height;
        if width == 0 || viewport_height == 0 {
            self.expand_level = level;
            self.invalidate_height_cache();
            return;
        }

        self.ensure_height_cache(width, viewport_height);
        let anchor = self.viewport_top_anchor();

        self.expand_level = level;
        self.invalidate_height_cache();
        self.ensure_height_cache(width, viewport_height);

        let max_scroll = self
            .total_content_height(width, viewport_height)
            .saturating_sub(viewport_height);
        let anchored = match anchor {
            Some((idx, offset_into_block)) if idx < self.render_cache.heights.len() => {
                let start = self.block_start_offset(idx);
                let height = self.render_cache.heights[idx].saturating_sub(start);
                start + offset_into_block.min(height)
            }
            _ => self.scroll_offset,
        };
        self.scroll_offset = anchored.min(max_scroll);
    }

    /// Index of the block occupying the top viewport row plus how far into
    /// that block the row sits. `None` when the cache is empty or the
    /// viewport starts past the end of history.
    fn viewport_top_anchor(&self) -> Option<(usize, usize)> {
        let heights = &self.render_cache.heights;
        let idx = heights.partition_point(|end| *end <= self.scroll_offset);
        if idx >= heights.len() {
            return None;
        }
        Some((idx, self.scroll_offset - self.block_start_offset(idx)))
    }

    pub fn scroll_up(&mut self, amount: usize) {
        if self.follows_output() {
            if self.render_cache.width > 0 && self.render_cache.viewport_height > 0 {
                self.scroll_offset = self.follow_scroll_offset(
                    self.render_cache.width,
                    self.render_cache.viewport_height,
                );
            } else {
                self.scroll_offset = 0;
            }
        }
        self.scroll_offset = self.scroll_offset.saturating_sub(amount);
        self.follow_mode = FollowOutputMode::Manual;
        self.dirty = true;
    }

    pub fn scroll_down(&mut self, amount: usize, viewport_height: usize, viewport_width: usize) {
        let total = self.total_content_height(viewport_width, viewport_height);
        let max_scroll = total.saturating_sub(viewport_height);
        if self.follows_output() {
            self.scroll_offset = self.follow_scroll_offset(viewport_width, viewport_height);
        }
        self.scroll_offset = self.scroll_offset.saturating_add(amount).min(max_scroll);
        if self.scroll_offset >= max_scroll {
            self.follow_mode = FollowOutputMode::PinnedBottom;
        } else {
            self.follow_mode = FollowOutputMode::Manual;
        }
        self.dirty = true;
    }

    pub fn scroll_to_bottom(&mut self) {
        if !self.follows_output() {
            return;
        }
        self.scroll_offset = usize::MAX;
    }

    pub fn resume_follow_output(&mut self) {
        self.follow_mode = FollowOutputMode::PinnedBottom;
        self.scroll_offset = usize::MAX;
        self.dirty = true;
    }

    pub fn resume_contextual_follow_output(&mut self) {
        self.follow_mode = FollowOutputMode::PinnedTurnStart;
        self.scroll_offset = usize::MAX;
        self.dirty = true;
    }

    pub fn refresh_scroll_position(&mut self, viewport_width: usize, viewport_height: usize) {
        let total = self.total_content_height(viewport_width, viewport_height);
        let max_scroll = total.saturating_sub(viewport_height);
        if self.follow_mode == FollowOutputMode::Manual {
            self.scroll_offset = self.scroll_offset.min(max_scroll);
            return;
        }
        self.scroll_offset = self.follow_scroll_offset(viewport_width, viewport_height);
    }

    pub fn keep_latest_user_block_visible(&mut self) {
        if self.follow_mode != FollowOutputMode::PinnedTurnStart {
            return;
        }

        let Some(last_idx) = self
            .timeline
            .iter()
            .rposition(|block| matches!(block, UiTimelineItem::UserInput(_)))
        else {
            self.scroll_to_bottom();
            return;
        };

        if self.render_cache.width == 0 || self.render_cache.viewport_height == 0 {
            self.scroll_to_bottom();
            return;
        }

        let width = self.render_cache.width;
        let viewport_height = self.render_cache.viewport_height;
        self.ensure_height_cache(width, viewport_height);

        let total_height = self.total_content_height(width, viewport_height);
        let max_scroll = total_height.saturating_sub(viewport_height);
        let block_start = self.block_start_offset(last_idx);
        let block_end = self.render_cache.heights[last_idx];
        let block_height = block_end.saturating_sub(block_start);
        let block_content_start = self.block_content_start_offset(last_idx);
        let has_splash_before = self.timeline[..last_idx]
            .iter()
            .any(|block| matches!(block, UiTimelineItem::Splash));

        let awaiting_first_visible_output = self
            .live
            .turn
            .as_ref()
            .is_some_and(|turn| !turn.has_visible_output);

        self.scroll_offset = if awaiting_first_visible_output
            && (has_splash_before || block_height >= viewport_height)
        {
            self.contextual_follow_offset(block_content_start, max_scroll)
        } else {
            block_end.saturating_sub(viewport_height).min(max_scroll)
        };
    }

    fn follow_scroll_offset(&mut self, viewport_width: usize, viewport_height: usize) -> usize {
        let total_height = self.total_content_height(viewport_width, viewport_height);
        let max_scroll = total_height.saturating_sub(viewport_height);

        match self.follow_mode {
            FollowOutputMode::Manual => return self.scroll_offset.min(max_scroll),
            FollowOutputMode::PinnedBottom => return max_scroll,
            FollowOutputMode::PinnedTurnStart => {}
        }

        if !self.turn_active() {
            return max_scroll;
        }

        let awaiting_first_visible_output = self
            .live
            .turn
            .as_ref()
            .is_some_and(|turn| !turn.has_visible_output);

        if awaiting_first_visible_output {
            return self.latest_user_block_anchor_offset(max_scroll);
        }

        let anchor_output_start = self
            .live
            .turn
            .as_ref()
            .is_some_and(|turn| turn.output_start_anchor_pending);

        if anchor_output_start {
            if let Some(turn) = self.live.turn.as_mut() {
                turn.output_start_anchor_pending = false;
            }
            self.follow_mode = FollowOutputMode::PinnedBottom;

            let Some(output_start) = self.latest_turn_output_start_offset() else {
                return max_scroll;
            };

            return self.contextual_follow_offset(output_start, max_scroll);
        }

        max_scroll
    }

    pub(super) fn latest_turn_output_start_offset(&self) -> Option<usize> {
        let search_start = self
            .timeline
            .iter()
            .rposition(|block| matches!(block, UiTimelineItem::UserInput(_)))
            .map(|idx| idx + 1)
            .unwrap_or(0);

        if let Some(idx) = self.timeline[search_start..]
            .iter()
            .position(Self::is_turn_visible_output_block)
            .map(|offset| search_start + offset)
        {
            return Some(self.block_content_start_offset(idx));
        }

        let history_tail = self.render_cache.heights.last().copied().unwrap_or(0);
        if self.live.tool_output.height() > 0
            && self.live.tool_output.title.is_some()
            && self.live_tool_output_anchor_block_index().is_none()
        {
            return Some(history_tail);
        }
        if self.live.reasoning.has_renderable_output() {
            return Some(history_tail + self.live_reasoning_leading_padding());
        }
        self.live
            .assistant
            .has_renderable_output()
            .then_some(history_tail + self.live_assistant_leading_padding())
    }

    fn is_turn_visible_output_block(block: &UiTimelineItem) -> bool {
        matches!(
            block,
            UiTimelineItem::AssistantText(_)
                | UiTimelineItem::AssistantReasoning(_)
                | UiTimelineItem::Activity(_)
                | UiTimelineItem::ShellOutput { .. }
                | UiTimelineItem::Error(_)
                | UiTimelineItem::PluginPanel(_)
        )
    }

    fn latest_user_block_anchor_offset(&self, max_scroll: usize) -> usize {
        let Some(last_idx) = self
            .timeline
            .iter()
            .rposition(|block| matches!(block, UiTimelineItem::UserInput(_)))
        else {
            return max_scroll;
        };

        self.contextual_follow_offset(self.block_content_start_offset(last_idx), max_scroll)
    }

    pub(super) fn contextual_follow_offset(
        &self,
        content_start: usize,
        max_scroll: usize,
    ) -> usize {
        content_start
            .saturating_sub(FOLLOW_OUTPUT_CONTEXT_LINES)
            .min(max_scroll)
    }

    fn follows_output(&self) -> bool {
        self.follow_mode != FollowOutputMode::Manual
    }

    fn block_start_offset(&self, idx: usize) -> usize {
        if idx == 0 {
            0
        } else {
            self.render_cache.heights[idx - 1]
        }
    }

    pub(super) fn block_content_start_offset(&self, idx: usize) -> usize {
        self.block_start_offset(idx) + self.block_leading_padding(idx)
    }

    fn block_leading_padding(&self, idx: usize) -> usize {
        if idx == 0 {
            return 0;
        }

        match self.timeline.get(idx) {
            Some(UiTimelineItem::UserInput(_)) => {
                usize::from(!matches!(self.timeline[idx - 1], UiTimelineItem::Splash))
            }
            Some(UiTimelineItem::AssistantText(_)) => usize::from(!matches!(
                self.timeline[idx - 1],
                UiTimelineItem::AssistantText(_) | UiTimelineItem::Splash
            )),
            _ => 0,
        }
    }

    pub fn ensure_height_cache_pub(&mut self, width: usize, viewport_height: usize) {
        self.ensure_height_cache(width, viewport_height);
        self.ensure_live_markdown_rendered(width);
    }

    pub fn height_cache_snapshot(&self) -> &[usize] {
        &self.render_cache.heights
    }

    fn ensure_height_cache(&mut self, width: usize, viewport_height: usize) {
        let dimensions_changed = self.render_cache.width != width
            || self.render_cache.viewport_height != viewport_height;
        if dimensions_changed {
            self.render_cache.heights.clear();
            self.render_cache.dirty_from = 0;
        }
        if !self.render_cache.heights.is_empty()
            && !dimensions_changed
            && self.render_cache.dirty_from >= self.timeline.len()
        {
            return;
        }
        self.render_cache.width = width;
        self.render_cache.viewport_height = viewport_height;

        let target_len = self.timeline.len();
        if self.render_cache.heights.len() > target_len {
            self.render_cache.heights.truncate(target_len);
        }
        let dirty_from = self.render_cache.dirty_from.min(target_len);
        if dirty_from == 0 {
            self.render_cache.heights.clear();
            self.render_cache.heights.reserve(target_len);
        } else {
            self.render_cache.heights.truncate(dirty_from);
        }
        let mut cumulative = if dirty_from == 0 {
            0
        } else {
            self.render_cache.heights[dirty_from - 1]
        };
        for i in dirty_from..target_len {
            cumulative += self.rendered_block_height_cached(i, width, viewport_height);
            self.render_cache.heights.push(cumulative);
        }
        self.render_cache.dirty_from = target_len;
    }

    pub fn total_content_height(&mut self, width: usize, viewport_height: usize) -> usize {
        self.ensure_height_cache(width, viewport_height);
        self.ensure_live_markdown_rendered(width);
        self.render_cache.heights.last().copied().unwrap_or(0)
            + crate::render::live_tool_output_standalone_height(self, width)
            + self.live_reasoning_height()
            + self.live_assistant_height()
            + crate::render::plan_dock_trailing_height(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WIDTH: usize = 40;
    const VIEWPORT: usize = 6;

    fn app_with_code_block() -> App {
        let mut app = App::new("test-model".into(), "test".into(), "test-session-id".into());
        let mut timeline = vec![
            UiTimelineItem::UserInput("first question".into()),
            UiTimelineItem::LashlangCode("one\ntwo\nthree\nfour\nfive\nsix".into()),
        ];
        // Enough trailing history that the anchored offset stays well
        // below `max_scroll` — otherwise clamping, not anchoring, would
        // decide the assertion.
        timeline.extend(
            (0..12).map(|idx| UiTimelineItem::UserInput(format!("follow-up question {idx}"))),
        );
        app.timeline = timeline.into();
        app.expand_level = 0;
        app.ensure_height_cache_pub(WIDTH, VIEWPORT);
        app
    }

    #[test]
    fn expand_level_applies_while_scrolled_up() {
        let mut app = app_with_code_block();
        // Park the viewport on the block right after the code block, in
        // manual scroll mode — the state Alt+O used to silently ignore.
        let block_two_start = app.height_cache_snapshot()[1];
        app.scroll_offset = block_two_start;
        app.follow_mode = FollowOutputMode::Manual;

        app.set_expand_level(2);

        assert_eq!(
            app.expand_level, 2,
            "expand level must apply while scrolled"
        );
        assert_eq!(
            app.follow_mode,
            FollowOutputMode::Manual,
            "expanding must not silently resume follow mode",
        );
        let expanded_block_two_start = app.height_cache_snapshot()[1];
        assert!(
            expanded_block_two_start > block_two_start,
            "code block should have grown at full expansion",
        );
        assert_eq!(
            app.scroll_offset, expanded_block_two_start,
            "viewport should stay anchored to the same block after re-layout",
        );
    }

    #[test]
    fn collapsing_while_scrolled_clamps_to_max_scroll() {
        let mut app = app_with_code_block();
        app.expand_level = 2;
        app.invalidate_height_cache();
        app.ensure_height_cache_pub(WIDTH, VIEWPORT);
        app.scroll_offset = app.total_content_height(WIDTH, VIEWPORT);
        app.follow_mode = FollowOutputMode::Manual;

        app.set_expand_level(0);

        let max_scroll = app
            .total_content_height(WIDTH, VIEWPORT)
            .saturating_sub(VIEWPORT);
        assert_eq!(app.expand_level, 0);
        assert_eq!(app.follow_mode, FollowOutputMode::Manual);
        assert!(
            app.scroll_offset <= max_scroll,
            "scroll offset {} exceeds max scroll {max_scroll} after collapsing",
            app.scroll_offset,
        );
    }

    #[test]
    fn clear_drops_previous_session_activity_journal() {
        let mut app = App::new("test-model".into(), "test".into(), "test-session-id".into());
        let activity = ActivityBlock::new(
            ActivityKind::ShellCommand,
            "bash",
            serde_json::json!({ "command": "nproc" }),
            "nproc",
            ActivityStatus::Completed,
            serde_json::json!({ "output": "16\n" }),
            5,
        );
        app.journal_lashlang_activity(Some((0, 0)), &activity);
        assert!(!app.ui_activity_journal.is_empty());
        assert!(!app.pending_ui_activity_records().is_empty());

        app.clear();

        assert!(app.ui_activity_journal.is_empty());
        assert!(app.pending_ui_activity_records().is_empty());
    }
}
