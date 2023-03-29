lazy_static::lazy_static! {
    static ref TIME: std::time::Instant = std::time::Instant::now();
}

/// Elapsed time from an unspecified point in the past. This time point is fixed
/// during the lifetime of the program.
pub fn elapsed() -> std::time::Duration {
    TIME.elapsed()
}
