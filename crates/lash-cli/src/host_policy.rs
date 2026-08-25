//! Explicit operational policy chosen by the lash CLI host.

/// Maximum protocol steps one user turn may execute.
pub(crate) const TURN_BUDGET: usize = 128;

/// Maximum consecutive provider attempts that make no durable progress.
pub(crate) const NO_PROGRESS_BUDGET: usize = 8;

/// Maximum logical bytes and graph rows accepted by one atomic runtime commit.
pub(crate) const COMMIT_BUDGET: (usize, usize) = (1024 * 1024, 512);

/// Model-action token reserve used when Lash batches queued work.
pub(crate) const QUEUED_WORK_BATCHING: usize = 1024;

/// Finite RLM VM limits, spanning instructions, wall time, and memory.
pub(crate) const RLM_EXECUTION_BOUNDS: RlmExecutionBounds = RlmExecutionBounds {
    instructions: 1_000_000,
    wall_clock_seconds: 30,
    memory_mebibytes: 64,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RlmExecutionBounds {
    pub(crate) instructions: u64,
    pub(crate) wall_clock_seconds: u64,
    pub(crate) memory_mebibytes: u64,
}

pub(crate) fn turn_budget() -> lash::TurnBudget {
    lash::TurnBudget::bounded(TURN_BUDGET)
}

pub(crate) fn no_progress_budget() -> lash::NoProgressBudget {
    lash::NoProgressBudget::bounded(NO_PROGRESS_BUDGET)
}

pub(crate) fn commit_budget() -> lash::CommitBudget {
    lash::CommitBudget::bounded(COMMIT_BUDGET.0, COMMIT_BUDGET.1)
}

pub(crate) fn queued_work_batching() -> lash::QueuedWorkBatchingConfig {
    lash::QueuedWorkBatchingConfig::new(QUEUED_WORK_BATCHING)
}

pub(crate) fn rlm_protocol_config() -> lash::rlm::RlmProtocolPluginConfig {
    lash::rlm::RlmProtocolPluginConfig::builder()
        .instruction_limit(lash::rlm::InstructionBound::instructions(
            RLM_EXECUTION_BOUNDS.instructions,
        ))
        .wall_clock(lash::rlm::WallClockBound::secs(
            RLM_EXECUTION_BOUNDS.wall_clock_seconds,
        ))
        .memory_limit(lash::rlm::MemoryBound::mebibytes(
            RLM_EXECUTION_BOUNDS.memory_mebibytes,
        ))
        .build()
}
