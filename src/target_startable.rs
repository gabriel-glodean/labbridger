/// A managed target that can be commanded to start
/// (e.g. Wake-on-LAN, an out-of-band power API, etc.).
/// Implement this for targets whose status is `Offline`.
pub trait Startable {
    /// Issue the start command. Returns an error description on failure.
    async fn start(&self) -> Result<(), String>;
}

