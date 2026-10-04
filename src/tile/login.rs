//! A question `ssh` asks while a source logs in, answered in the tile of that source.

use crate::askpass::{Ask, Kind};

use super::{Content, Tile};

/// The question being asked and what has been typed so far.
pub(super) struct Asking {
    ask: Ask,
    typed: String,
}

impl Asking {
    pub(super) fn new(ask: Ask) -> Self {
        Self {
            ask,
            typed: String::new(),
        }
    }

    /// Gives up the question: `ssh` is told there is no answer.
    pub(super) fn cancel(self) {
        self.ask.answer(None);
    }
}

/// What the tile draws for a question: the text of the question, and the answer as typed,
/// hidden if it is a secret.
pub struct Question<'a> {
    pub prompt: &'a str,
    pub shown: String,
}

impl Tile {
    /// Is a question waiting for an answer in this tile?
    pub fn is_asking(&self) -> bool {
        matches!(
            self.content,
            Content::Source {
                asking: Some(_),
                ..
            }
        )
    }

    /// The question to draw, if there is one.
    pub fn question(&self) -> Option<Question<'_>> {
        let Content::Source {
            asking: Some(asking),
            ..
        } = &self.content
        else {
            return None;
        };
        let shown = match asking.ask.kind {
            // A password is not shown, not even how long it is: one dot per typed character
            // would give that away, but a count of dots is what every login box shows.
            Kind::Secret => "•".repeat(asking.typed.chars().count()),
            Kind::Confirm | Kind::Info => asking.typed.clone(),
        };
        Some(Question {
            prompt: &asking.ask.prompt,
            shown,
        })
    }

    fn typed_mut(&mut self) -> Option<&mut String> {
        match &mut self.content {
            Content::Source {
                asking: Some(asking),
                ..
            } => Some(&mut asking.typed),
            _ => None,
        }
    }

    pub fn type_char(&mut self, c: char) {
        if let Some(typed) = self.typed_mut() {
            typed.push(c);
        }
    }

    pub fn erase_char(&mut self) {
        if let Some(typed) = self.typed_mut() {
            typed.pop();
        }
    }

    /// Sends what was typed to `ssh` and closes the question.
    pub fn submit_answer(&mut self) {
        if let Content::Source { asking, .. } = &mut self.content
            && let Some(Asking { ask, typed }) = asking.take()
        {
            ask.answer(Some(typed));
        }
    }

    /// Closes the question without an answer: the login fails.
    pub fn cancel_answer(&mut self) {
        if let Content::Source { asking, .. } = &mut self.content
            && let Some(Asking { ask, .. }) = asking.take()
        {
            ask.answer(None);
        }
    }

    /// Puts a question in the tile as if `ssh` had asked it.
    #[cfg(test)]
    pub fn ask_for_test(&mut self, ask: Ask) {
        if let Content::Source { asking, .. } = &mut self.content {
            *asking = Some(Asking::new(ask));
        }
    }
}
