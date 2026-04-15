use crate::target_status::TargetStatus;

/// A managed target whose readiness can be polled without side-effects.
/// Use this to drive the `Offline → Starting → Online` transition.
pub trait Probeable {
    /// Returns the current live [`TargetStatus`] of this target.
    async fn probe(&self) -> TargetStatus;
}

