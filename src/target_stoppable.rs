/// A managed target that can be commanded to shut down.
#[allow(dead_code)]
pub trait Stoppable {
    /// Issue the shutdown/stop command. Returns an error description on failure.
    async fn stop(&self) -> Result<(), String>;
}

