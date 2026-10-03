use crate::lines::Lines;

/// Which part of the buffer is visible: either "stuck to the end" (follow) or pinned
/// to a fixed line.
pub struct Viewport {
    /// Follow mode: always show the newest lines, like `tail -f`.
    follow: bool,
    /// Sequence number of the top visible line. Only meaningful while `follow` is false.
    top: u64,
}

impl Viewport {
    pub fn new() -> Self {
        Self {
            follow: true,
            top: 0,
        }
    }

    pub fn is_following(&self) -> bool {
        self.follow
    }

    /// Sequence number of the first line to draw, for a window `height` lines tall.
    // `&dyn Lines` accepts a reference to *any* type implementing the trait (dynamic dispatch,
    // like calling through a Java interface). The viewport doesn't care where lines live.
    pub fn top(&self, buf: &dyn Lines, height: usize) -> u64 {
        let max_top = Self::max_top(buf, height);
        if self.follow {
            max_top
        } else {
            // The pinned line may have been evicted since; never go below the oldest line.
            self.top.clamp(buf.first_seq().min(max_top), max_top)
        }
    }

    pub fn scroll_up(&mut self, buf: &dyn Lines, height: usize, n: u64) {
        let current = self.top(buf, height);
        self.follow = false;
        self.top = current.saturating_sub(n).max(buf.first_seq());
    }

    pub fn scroll_down(&mut self, buf: &dyn Lines, height: usize, n: u64) {
        let target = self.top(buf, height) + n;
        if target >= Self::max_top(buf, height) {
            self.follow = true; // scrolled back to the bottom: resume following
        } else {
            self.follow = false;
            self.top = target;
        }
    }

    pub fn jump_to_start(&mut self, buf: &dyn Lines) {
        self.follow = false;
        self.top = buf.first_seq();
    }

    /// Pins the top of the view to `seq`. Out-of-range values are clamped by `top()`.
    pub fn jump_to(&mut self, seq: u64) {
        self.follow = false;
        self.top = seq;
    }

    pub fn jump_to_end(&mut self) {
        self.follow = true;
    }

    /// The lowest the view can start: the last `height` lines fill it, but never above the
    /// first line. (`saturating_sub` alone only stops at 0, and sequence numbers don't start
    /// there, so a log shorter than the window would get a top *above* its first line.)
    fn max_top(buf: &dyn Lines, height: usize) -> u64 {
        buf.end_seq()
            .saturating_sub(height as u64)
            .max(buf.first_seq())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::RingBuffer;

    fn buffer_with(capacity: usize, lines: u32) -> RingBuffer {
        let mut buf = RingBuffer::with_origin(capacity, 0);
        for i in 0..lines {
            buf.push(0, format!("line {i}"));
        }
        buf
    }

    #[test]
    fn follow_mode_shows_the_newest_lines() {
        let buf = buffer_with(100, 50);
        assert_eq!(Viewport::new().top(&buf, 10), 40);
    }

    #[test]
    fn paused_view_stays_put_while_new_lines_arrive() {
        let mut buf = buffer_with(100, 50);
        let mut view = Viewport::new();
        view.scroll_up(&buf, 10, 5);
        assert_eq!(view.top(&buf, 10), 35);

        for i in 0..20 {
            buf.push(0, format!("new {i}"));
        }
        assert_eq!(view.top(&buf, 10), 35);
        assert!(!view.is_following());
    }

    #[test]
    fn paused_view_is_pushed_forward_when_its_line_is_evicted() {
        let mut buf = buffer_with(20, 20);
        let mut view = Viewport::new();
        view.jump_to_start(&buf);
        for i in 0..5 {
            buf.push(0, format!("new {i}"));
        }
        assert_eq!(view.top(&buf, 10), 5); // lines 0..5 are gone
    }

    #[test]
    fn scrolling_to_the_bottom_resumes_following() {
        let buf = buffer_with(100, 50);
        let mut view = Viewport::new();
        view.scroll_up(&buf, 10, 3);
        assert!(!view.is_following());
        view.scroll_down(&buf, 10, 3);
        assert!(view.is_following());
    }

    #[test]
    fn a_log_shorter_than_the_window_starts_at_its_first_line() {
        // Sequence numbers start far from 0, as in the running program.
        let mut buf = RingBuffer::new(100);
        for i in 0..3 {
            buf.push(0, format!("line {i}"));
        }
        let mut view = Viewport::new();
        assert_eq!(view.top(&buf, 20), buf.first_seq());

        // Scrolling around in it must neither panic nor leave the log.
        view.scroll_up(&buf, 20, 5);
        assert_eq!(view.top(&buf, 20), buf.first_seq());
        view.scroll_down(&buf, 20, 5);
        assert!(view.is_following());
        view.jump_to_start(&buf);
        assert_eq!(view.top(&buf, 20), buf.first_seq());
    }

    #[test]
    fn short_buffer_does_not_underflow() {
        let buf = buffer_with(100, 3);
        let mut view = Viewport::new();
        view.scroll_up(&buf, 10, 5);
        assert_eq!(view.top(&buf, 10), 0);
    }
}
