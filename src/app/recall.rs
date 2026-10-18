//! What was typed at the prompts before, to be typed again: Up and Down (or Ctrl+P and Ctrl+N)
//! step through it, like the history of a shell or of `less`. Each prompt (`/`, `&`, `:`) has a
//! list of its own, kept for as long as tilog runs.

use super::keys::Prompt;

/// The most entries a prompt remembers.
const MAX_ENTRIES: usize = 100;

#[derive(Default)]
pub(super) struct Recall {
    search: Vec<String>,
    filter: Vec<String>,
    command: Vec<String>,
    /// While stepping through a list: where in it, and what was typed before stepping began
    /// (stepping past the newest entry gives it back).
    browsing: Option<(usize, String)>,
}

impl Recall {
    fn list(&mut self, prompt: Prompt) -> &mut Vec<String> {
        match prompt {
            Prompt::Search => &mut self.search,
            Prompt::Filter => &mut self.filter,
            Prompt::Command => &mut self.command,
        }
    }

    /// Remembers `text`, entered at `prompt`, as the newest entry. An earlier copy of the same
    /// text is dropped, so the list has no repeats.
    pub(super) fn remember(&mut self, prompt: Prompt, text: &str) {
        self.browsing = None;
        if text.is_empty() {
            return;
        }
        let list = self.list(prompt);
        list.retain(|entry| entry != text);
        list.push(text.to_string());
        if list.len() > MAX_ENTRIES {
            list.remove(0);
        }
    }

    /// The entry before the one shown, starting from the newest. `typed` is what the prompt
    /// holds now; it comes back after the newest entry. `None` if there is nothing older.
    pub(super) fn older(&mut self, prompt: Prompt, typed: &str) -> Option<String> {
        let position = match self.browsing.take() {
            Some((0, draft)) => {
                self.browsing = Some((0, draft));
                return None; // already at the oldest
            }
            Some((position, draft)) => {
                self.browsing = Some((position - 1, draft));
                position - 1
            }
            None => {
                let newest = self.list(prompt).len().checked_sub(1)?;
                self.browsing = Some((newest, typed.to_string()));
                newest
            }
        };
        self.list(prompt).get(position).cloned()
    }

    /// The entry after the one shown, or what was typed before stepping began once past the
    /// newest. `None` if not stepping through the list.
    pub(super) fn newer(&mut self, prompt: Prompt) -> Option<String> {
        let (position, draft) = self.browsing.take()?;
        if position + 1 < self.list(prompt).len() {
            self.browsing = Some((position + 1, draft));
            return self.list(prompt).get(position + 1).cloned();
        }
        Some(draft)
    }

    /// Typing (or opening a prompt) ends the stepping: the next Up starts from the newest.
    pub(super) fn stop(&mut self) {
        self.browsing = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_back_and_forth_and_gives_back_what_was_typed() {
        let mut recall = Recall::default();
        for text in ["ERROR", "timeout", "user=alice"] {
            recall.remember(Prompt::Search, text);
        }
        assert_eq!(
            recall.older(Prompt::Search, "draft").as_deref(),
            Some("user=alice")
        );
        assert_eq!(recall.older(Prompt::Search, "").as_deref(), Some("timeout"));
        assert_eq!(recall.older(Prompt::Search, "").as_deref(), Some("ERROR"));
        assert_eq!(
            recall.older(Prompt::Search, ""),
            None,
            "nothing older than the oldest"
        );
        assert_eq!(recall.newer(Prompt::Search).as_deref(), Some("timeout"));
        assert_eq!(recall.newer(Prompt::Search).as_deref(), Some("user=alice"));
        assert_eq!(
            recall.newer(Prompt::Search).as_deref(),
            Some("draft"),
            "back to what was typed"
        );
        assert_eq!(recall.newer(Prompt::Search), None, "not stepping any more");
    }

    #[test]
    fn each_prompt_has_its_own_list_without_repeats() {
        let mut recall = Recall::default();
        recall.remember(Prompt::Search, "a");
        recall.remember(Prompt::Filter, "b");
        recall.remember(Prompt::Search, "c");
        recall.remember(Prompt::Search, "a"); // again: it moves to the end, once
        recall.remember(Prompt::Search, "");
        assert_eq!(recall.search, ["c", "a"]);
        assert_eq!(recall.filter, ["b"]);
        assert_eq!(
            recall.older(Prompt::Command, ""),
            None,
            "nothing typed at : yet"
        );
    }

    #[test]
    fn the_list_is_bounded() {
        let mut recall = Recall::default();
        for i in 0..150 {
            recall.remember(Prompt::Command, &format!("goto {i}"));
        }
        assert_eq!(recall.command.len(), MAX_ENTRIES);
        assert_eq!(recall.command[0], "goto 50");
    }
}
