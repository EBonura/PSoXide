//! Deterministic frame/task scheduler.
//!
//! The scheduler is deliberately cooperative: it does not spawn workers or
//! allocate job records. PS1 code runs on one CPU, so the useful abstraction is
//! deciding which fixed-size pieces of work are allowed to run on each VBlank.
//!
//! The app runner currently wires two engine tasks through this scheduler:
//! fixed simulation/update and visual render/present. The public task
//! descriptors are the same vocabulary future systems should use for streaming,
//! visibility refreshes, particles, and editor/debug maintenance.

use crate::frames::SimTick;

/// Stable id for one scheduled task.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TaskId(u8);

impl TaskId {
    /// Build a task id from a compact numeric slot.
    pub const fn new(id: u8) -> Self {
        Self(id)
    }

    /// Raw numeric task slot.
    pub const fn as_u8(self) -> u8 {
        self.0
    }
}

/// Built-in task used by [`crate::App`] for `Scene::update`.
pub const TASK_FIXED_UPDATE: TaskId = TaskId::new(0);
/// Built-in task used by [`crate::App`] for `Scene::render` + present.
pub const TASK_VISUAL_RENDER: TaskId = TaskId::new(1);

/// Broad execution lane for a scheduled task.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum TaskLane {
    /// Gameplay-critical fixed work that must keep up with VBlank time.
    FixedCritical,
    /// Fixed work that should run after critical gameplay.
    FixedPost,
    /// Required visual work for a frame that will be presented.
    VisualCritical,
    /// Visual work that may be disabled or lowered by project tuning.
    VisualOptional,
    /// Incremental streaming/cache work that consumes an explicit budget.
    StreamingBudget,
    /// Low-priority cleanup, telemetry, and editor-only maintenance.
    Maintenance,
}

/// Manual cadence for a task.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum TaskCadence {
    /// Run every fixed simulation tick.
    EveryTick,
    /// Run once every `n` fixed ticks. `0` is treated as disabled.
    EveryNTicks(u16),
    /// Run only when an owner marks the task dirty.
    WhenDirty,
    /// Run only when the scheduler has spare budget for this lane.
    WhenBudgetAllows,
    /// Do not run.
    Disabled,
}

impl TaskCadence {
    /// Returns true when this cadence is due on `tick`.
    pub fn due_on_tick(self, tick: SimTick) -> bool {
        match self {
            Self::EveryTick => true,
            Self::EveryNTicks(n) => n != 0 && tick.as_u32().is_multiple_of(n as u32),
            Self::WhenDirty | Self::WhenBudgetAllows | Self::Disabled => false,
        }
    }
}

/// Behaviour when the scheduler is late.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum OverloadPolicy {
    /// Run every missed tick before moving on.
    MustCatchUp,
    /// Collapse missed work into one latest-state run.
    CollapseToLatest,
    /// Skip the run if the owning lane is already late.
    DropIfLate,
    /// Resume a bounded amount of work on later ticks.
    BudgetSlice,
}

/// Optional per-run budget. The app runner does not enforce cycle budgets yet;
/// task descriptions carry them so streaming/cache jobs can opt in without a
/// second scheduling vocabulary.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum TaskBudget {
    /// No explicit budget.
    Unbounded,
    /// Approximate guest CPU cycle budget.
    Cycles(u32),
    /// Fixed number of records/items to process.
    WorkItems(u16),
}

/// Static description for one scheduled task.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct TaskDescriptor {
    /// Stable task id.
    pub id: TaskId,
    /// Execution lane.
    pub lane: TaskLane,
    /// Manual cadence.
    pub cadence: TaskCadence,
    /// Lower values run earlier within a lane.
    pub priority: u8,
    /// Late-frame behaviour.
    pub overload: OverloadPolicy,
    /// Optional work budget.
    pub budget: TaskBudget,
}

impl TaskDescriptor {
    /// Build a task descriptor with common defaults.
    pub const fn new(id: TaskId, lane: TaskLane, cadence: TaskCadence) -> Self {
        Self {
            id,
            lane,
            cadence,
            priority: 128,
            overload: OverloadPolicy::DropIfLate,
            budget: TaskBudget::Unbounded,
        }
    }

    /// Set task priority.
    pub const fn with_priority(mut self, priority: u8) -> Self {
        self.priority = priority;
        self
    }

    /// Set late-frame policy.
    pub const fn with_overload_policy(mut self, overload: OverloadPolicy) -> Self {
        self.overload = overload;
        self
    }

    /// Set task budget.
    pub const fn with_budget(mut self, budget: TaskBudget) -> Self {
        self.budget = budget;
        self
    }
}

/// Collect due task ids for one lane into caller-owned storage.
///
/// Descriptors are emitted in slice order. Keep static descriptor tables sorted
/// by lane and priority at authoring time; the runtime path intentionally does
/// no sorting or allocation.
pub fn collect_due_tasks(
    descriptors: &[TaskDescriptor],
    lane: TaskLane,
    tick: SimTick,
    out: &mut [TaskId],
) -> usize {
    let mut count = 0usize;
    for descriptor in descriptors {
        if descriptor.lane != lane || !descriptor.cadence.due_on_tick(tick) {
            continue;
        }
        let Some(slot) = out.get_mut(count) else {
            break;
        };
        *slot = descriptor.id;
        count += 1;
    }
    count
}

/// Manual scheduler tuning used by the app runner.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct SchedulerConfig {
    /// Maximum fixed updates to run after a visual has become due before
    /// presenting anyway. Higher values preserve gameplay speed under render
    /// load; lower values reduce black-screen risk under catastrophic overload.
    ///
    /// `0` means "match the visual cadence": a 30 Hz visual target on a
    /// 60 Hz display runs at most two fixed ticks before presenting.
    pub max_fixed_ticks_before_visual: u16,
}

impl SchedulerConfig {
    /// Cadence-aware default: match the visual interval supplied to
    /// [`FrameScheduler::new`].
    pub const DEFAULT_MAX_FIXED_TICKS_BEFORE_VISUAL: u16 = 0;

    /// Build default scheduler tuning.
    pub const fn new() -> Self {
        Self {
            max_fixed_ticks_before_visual: Self::DEFAULT_MAX_FIXED_TICKS_BEFORE_VISUAL,
        }
    }

    /// Set the fixed-update burst limit.
    pub const fn with_max_fixed_ticks_before_visual(mut self, max_ticks: u16) -> Self {
        self.max_fixed_ticks_before_visual = max_ticks;
        self
    }

    const fn normalized(self, visual_interval: u16) -> Self {
        Self {
            max_fixed_ticks_before_visual: if self.max_fixed_ticks_before_visual == 0 {
                visual_interval
            } else {
                self.max_fixed_ticks_before_visual
            },
        }
    }
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// Scheduler decision returned to the app runner.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum SchedulerAction {
    /// No work is ready; wait for the next hardware VBlank.
    WaitForVBlank,
    /// Run one fixed simulation tick.
    RunFixedUpdate {
        /// Simulation tick to expose to the scene.
        tick: SimTick,
    },
    /// Render and present one visual frame.
    RunVisualFrame {
        /// Visual intervals that were due but not presented.
        missed_visual_intervals: u16,
        /// True when the fixed-update burst limit forced the render while
        /// simulation ticks were still pending.
        fixed_update_clamped: bool,
    },
}

/// Result of completing one fixed update.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct FixedUpdateOutcome {
    /// Number of visual intervals that became due on this tick.
    pub visual_intervals_due: u16,
}

/// Pure VBlank-driven scheduler state.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct FrameScheduler {
    config: SchedulerConfig,
    visual_interval: u16,
    next_fixed_tick: u32,
    next_visual_tick: u32,
    due_visual_intervals: u16,
    fixed_ticks_since_visual: u16,
}

impl FrameScheduler {
    /// Build scheduler state for a visual cadence expressed in VBlanks.
    pub const fn new(config: SchedulerConfig, visual_interval: u16) -> Self {
        let visual_interval = if visual_interval == 0 {
            1
        } else {
            visual_interval
        };
        let config = config.normalized(visual_interval);
        Self {
            config,
            visual_interval,
            next_fixed_tick: 0,
            next_visual_tick: 0,
            due_visual_intervals: 0,
            fixed_ticks_since_visual: 0,
        }
    }

    /// Next action to run for the supplied elapsed VBlank count.
    pub fn next_action(&self, elapsed_vblank_ticks: u32) -> SchedulerAction {
        let fixed_ticks_ready = self.next_fixed_tick <= elapsed_vblank_ticks;
        let visual_due = self.due_visual_intervals != 0;
        let fixed_burst_open = !visual_due
            || self.fixed_ticks_since_visual < self.config.max_fixed_ticks_before_visual;

        if fixed_ticks_ready && fixed_burst_open {
            return SchedulerAction::RunFixedUpdate {
                tick: SimTick::from_u32(self.next_fixed_tick),
            };
        }

        if visual_due {
            return SchedulerAction::RunVisualFrame {
                missed_visual_intervals: self.missed_visual_intervals(elapsed_vblank_ticks),
                fixed_update_clamped: fixed_ticks_ready,
            };
        }

        SchedulerAction::WaitForVBlank
    }

    /// Mark the fixed update returned by [`FrameScheduler::next_action`] as complete.
    pub fn complete_fixed_update(&mut self) -> FixedUpdateOutcome {
        let due = self.mark_due_visual_intervals(self.next_fixed_tick);
        self.due_visual_intervals = self.due_visual_intervals.saturating_add(due);
        self.next_fixed_tick = self.next_fixed_tick.wrapping_add(1);
        self.fixed_ticks_since_visual = self.fixed_ticks_since_visual.saturating_add(1);
        FixedUpdateOutcome {
            visual_intervals_due: due,
        }
    }

    /// Mark the visual frame returned by [`FrameScheduler::next_action`] as complete.
    pub fn complete_visual_frame(&mut self) {
        self.due_visual_intervals = 0;
        self.fixed_ticks_since_visual = 0;
    }

    /// Next fixed tick that has not completed.
    pub const fn next_fixed_tick(&self) -> SimTick {
        SimTick::from_u32(self.next_fixed_tick)
    }

    /// Pending visual intervals that have been simulated but not presented.
    pub const fn due_visual_intervals(&self) -> u16 {
        self.due_visual_intervals
    }

    fn mark_due_visual_intervals(&mut self, fixed_tick: u32) -> u16 {
        if fixed_tick < self.next_visual_tick {
            return 0;
        }
        let interval = self.visual_interval.max(1) as u32;
        let due = fixed_tick
            .wrapping_sub(self.next_visual_tick)
            .checked_div(interval)
            .unwrap_or(0)
            .saturating_add(1);
        self.next_visual_tick = self
            .next_visual_tick
            .saturating_add(due.saturating_mul(interval));
        due.min(u16::MAX as u32) as u16
    }

    fn missed_visual_intervals(&self, elapsed_vblank_ticks: u32) -> u16 {
        let pending_ticks = if self.next_fixed_tick <= elapsed_vblank_ticks {
            elapsed_vblank_ticks
                .wrapping_sub(self.next_fixed_tick)
                .saturating_add(1)
        } else {
            0
        };
        let pending_visuals = pending_ticks / self.visual_interval.max(1) as u32;
        self.due_visual_intervals
            .saturating_sub(1)
            .saturating_add(pending_visuals.min(u16::MAX as u32) as u16)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn complete_due_update(scheduler: &mut FrameScheduler, elapsed: u32) -> FixedUpdateOutcome {
        assert!(matches!(
            scheduler.next_action(elapsed),
            SchedulerAction::RunFixedUpdate { .. }
        ));
        scheduler.complete_fixed_update()
    }

    #[test]
    fn task_cadence_matches_fixed_ticks() {
        assert!(TaskCadence::EveryTick.due_on_tick(SimTick::from_u32(7)));
        assert!(TaskCadence::EveryNTicks(2).due_on_tick(SimTick::from_u32(8)));
        assert!(!TaskCadence::EveryNTicks(2).due_on_tick(SimTick::from_u32(9)));
        assert!(!TaskCadence::EveryNTicks(0).due_on_tick(SimTick::from_u32(0)));
        assert!(!TaskCadence::Disabled.due_on_tick(SimTick::from_u32(0)));
    }

    #[test]
    fn collect_due_tasks_filters_lane_and_cadence_without_sorting() {
        const TASKS: &[TaskDescriptor] = &[
            TaskDescriptor::new(
                TaskId::new(10),
                TaskLane::FixedCritical,
                TaskCadence::EveryTick,
            )
            .with_priority(10),
            TaskDescriptor::new(
                TaskId::new(11),
                TaskLane::FixedCritical,
                TaskCadence::EveryNTicks(2),
            )
            .with_priority(20),
            TaskDescriptor::new(
                TaskId::new(12),
                TaskLane::VisualOptional,
                TaskCadence::EveryTick,
            )
            .with_priority(30),
        ];
        let mut out = [TaskId::new(0); 4];

        let count = collect_due_tasks(
            TASKS,
            TaskLane::FixedCritical,
            SimTick::from_u32(4),
            &mut out,
        );

        assert_eq!(count, 2);
        assert_eq!(out[0], TaskId::new(10));
        assert_eq!(out[1], TaskId::new(11));
    }

    #[test]
    fn scheduler_renders_first_tick_immediately() {
        let mut scheduler = FrameScheduler::new(SchedulerConfig::new(), 2);

        let outcome = complete_due_update(&mut scheduler, 0);

        assert_eq!(outcome.visual_intervals_due, 1);
        assert_eq!(
            scheduler.next_action(0),
            SchedulerAction::RunVisualFrame {
                missed_visual_intervals: 0,
                fixed_update_clamped: false,
            }
        );
    }

    #[test]
    fn scheduler_catches_simulation_up_before_rendering() {
        let mut scheduler = FrameScheduler::new(SchedulerConfig::new(), 2);
        complete_due_update(&mut scheduler, 0);
        scheduler.complete_visual_frame();

        let mut updates = 0u32;
        while let SchedulerAction::RunFixedUpdate { .. } = scheduler.next_action(4) {
            scheduler.complete_fixed_update();
            updates += 1;
        }

        assert_eq!(updates, 2);
        assert_eq!(
            scheduler.next_action(4),
            SchedulerAction::RunVisualFrame {
                missed_visual_intervals: 1,
                fixed_update_clamped: true,
            }
        );
    }

    #[test]
    fn scheduler_default_fixed_burst_tracks_visual_interval() {
        let mut scheduler = FrameScheduler::new(SchedulerConfig::new(), 3);
        complete_due_update(&mut scheduler, 0);
        scheduler.complete_visual_frame();

        let mut updates = 0u32;
        while let SchedulerAction::RunFixedUpdate { .. } = scheduler.next_action(9) {
            scheduler.complete_fixed_update();
            updates += 1;
        }

        assert_eq!(updates, 3);
        assert!(matches!(
            scheduler.next_action(9),
            SchedulerAction::RunVisualFrame {
                fixed_update_clamped: true,
                ..
            }
        ));
    }

    #[test]
    fn scheduler_clamps_catastrophic_fixed_backlog() {
        let config = SchedulerConfig::new().with_max_fixed_ticks_before_visual(3);
        let mut scheduler = FrameScheduler::new(config, 2);
        complete_due_update(&mut scheduler, 0);
        scheduler.complete_visual_frame();

        let mut updates = 0u32;
        while let SchedulerAction::RunFixedUpdate { .. } = scheduler.next_action(20) {
            scheduler.complete_fixed_update();
            updates += 1;
        }

        assert_eq!(updates, 3);
        assert_eq!(
            scheduler.next_action(20),
            SchedulerAction::RunVisualFrame {
                missed_visual_intervals: 8,
                fixed_update_clamped: true,
            }
        );
    }

    #[test]
    fn scheduler_waits_when_no_tick_or_visual_is_ready() {
        let mut scheduler = FrameScheduler::new(SchedulerConfig::new(), 2);
        complete_due_update(&mut scheduler, 0);
        scheduler.complete_visual_frame();

        assert_eq!(scheduler.next_action(0), SchedulerAction::WaitForVBlank);
    }
}
