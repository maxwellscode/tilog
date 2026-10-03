/// Anything that can show lines in a tile: the in-memory ring buffer of the main tile, or a
/// filtered view that reads its lines back from disk.
///
/// A trait is Rust's interface. Positions are *sequence numbers*: `first_seq()` is the oldest
/// available line, `end_seq()` is one past the newest.
pub trait Lines {
    fn first_seq(&self) -> u64;
    fn end_seq(&self) -> u64;

    /// Up to `count` lines starting at `from_seq`.
    ///
    /// Returns owned `String`s rather than borrowed `&str`s: an implementation that reads from
    /// disk has nothing to borrow from. Only the handful of visible rows are ever requested,
    /// so the copies are cheap.
    fn range(&self, from_seq: u64, count: usize) -> Vec<String>;
}
