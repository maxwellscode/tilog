//! Prompt mode: the status line is a text box for a search, a filter or a command.

use super::Prompt;
use crate::app::{App, ArgKind, Candidates};
use crate::command;
use crate::command::Spec;
use crate::completion;
use crate::highlight::Highlight;
use crate::session;
use crate::tile::Find;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// The most matches the box lists, so a huge directory can't make every key press slow.
const MAX_CANDIDATES: usize = 300;

impl App {
    pub(super) fn open_prompt(&mut self, prompt: Prompt) {
        self.prompt = Some(prompt);
        self.input.clear();
        self.menu_selected = 0;
    }

    pub(super) fn close_prompt(&mut self) {
        self.prompt = None;
        self.input.clear();
    }

    pub(super) fn on_prompt_key(&mut self, prompt: Prompt, key: KeyEvent, height: usize) {
        // The items are `&'static`, so this Vec does not borrow `self`.
        let menu = self.menu_items();
        let menu_open = !menu.is_empty();
        let matches_open = !self.candidates.is_empty();
        let selected = self.menu_selected.min(menu.len().saturating_sub(1));
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        match key.code {
            KeyCode::Char('c') if ctrl => self.quit = true,
            KeyCode::Esc => self.close_prompt(),
            // Backspace on an empty prompt leaves it, like in `vim` and `less`.
            KeyCode::Backspace if self.input.is_empty() => self.close_prompt(),

            // The box of matches after Tab on an argument: arrows choose, Tab or Enter insert.
            KeyCode::Up if matches_open => self.candidates.step(-1),
            KeyCode::Down if matches_open => self.candidates.step(1),
            KeyCode::Tab | KeyCode::Enter if matches_open => self.insert_candidate(),

            KeyCode::Up if menu_open => {
                self.menu_selected = (selected + menu.len() - 1) % menu.len();
            }
            KeyCode::Down if menu_open => self.menu_selected = (selected + 1) % menu.len(),
            KeyCode::Tab if menu_open => self.complete(menu[selected]),
            // Tab after `add ` or `load `: complete the path or session name.
            KeyCode::Tab if self.completion_context().is_some() => self.complete_argument(),
            KeyCode::Enter if menu_open => self.complete_and_run(menu[selected], height),
            KeyCode::Enter => self.submit_prompt(prompt, height),

            // Everything else is text editing.
            _ => {
                self.input.handle_key(key);
                self.menu_selected = 0; // the list may have changed, start at the top again
            }
        }
    }

    /// Enter: close the prompt and do what it was for.
    pub(super) fn submit_prompt(&mut self, prompt: Prompt, height: usize) {
        let text = self.input.take();
        self.prompt = None;
        let text = text.trim();

        match prompt {
            Prompt::Search if text.is_empty() => {
                self.highlight = None;
                self.info("search cleared");
            }
            Prompt::Search => {
                self.highlight = Some(Highlight::literal(text));
                self.next_match(Find::First, height);
            }
            // `&` takes the same arguments as `:filter`, so `&-r err(or)?` works too.
            Prompt::Filter if !text.is_empty() => self.run_command_line(&format!("filter {text}")),
            Prompt::Command if !text.is_empty() => self.run_command_line(text),
            Prompt::Filter | Prompt::Command => {}
        }
    }

    /// While a search or filter is being typed, its text is marked live in every window.
    pub(in crate::app) fn refresh_preview(&mut self) {
        let text = self.input.text();
        let live = matches!(self.prompt, Some(Prompt::Search | Prompt::Filter))
            && !text.is_empty()
            // A filter may start with flags (`-r`), which are not part of the text.
            && !text.starts_with('-');
        self.preview = live.then(|| Highlight::literal(&text));
    }

    /// Commands to offer in the menu: shown while a command prompt holds a partial name,
    /// and gone as soon as a space starts the arguments.
    pub(in crate::app) fn menu_items(&self) -> Vec<&'static Spec> {
        if self.prompt != Some(Prompt::Command) {
            return Vec::new();
        }
        let text = self.input.text();
        if text.contains(char::is_whitespace) {
            return Vec::new();
        }
        command::matching(&text)
    }

    /// Tab: write the command's name into the prompt (plus a space if it takes arguments).
    pub(super) fn complete(&mut self, spec: &Spec) {
        let space = if spec.takes_args() { " " } else { "" };
        self.input.set(&format!("{}{space}", spec.name()));
    }

    /// Enter on a menu entry: run it, unless it can't run without arguments, then wait for them.
    pub(super) fn complete_and_run(&mut self, spec: &Spec, height: usize) {
        self.complete(spec);
        // A command that takes arguments, even optional ones (`follow [all]`), waits for them:
        // Enter again runs it as it is.
        if !spec.takes_args() {
            self.submit_prompt(Prompt::Command, height);
        }
    }

    /// If the prompt holds `add <partial>` (or `load`, `save`): what kind of argument it is,
    /// the text up to the argument (`add `), and the argument typed so far.
    pub(super) fn completion_context(&self) -> Option<(ArgKind, String, String)> {
        if self.prompt != Some(Prompt::Command) {
            return None;
        }
        let text = self.input.text();
        let split = text.find(char::is_whitespace)?;
        let partial = text[split..].trim_start();
        let kind = match text[..split].to_lowercase().as_str() {
            "add" | "a" => match partial.split_once(':') {
                // `ssh:host` completes the host; once the path part begins, nothing to complete.
                Some(("ssh", rest)) if !rest.contains(':') => ArgKind::Ssh,
                Some(("ssh" | "docker" | "kube" | "k8s" | "cmd", _)) => return None,
                _ => ArgKind::Path,
            },
            "write" | "w" if partial.starts_with('-') => return None,
            "write" | "w" => ArgKind::Path,
            "load" | "save" => ArgKind::Session,
            _ => return None,
        };
        let head = text[..text.len() - partial.len()].to_string();
        Some((kind, head, partial.to_string()))
    }

    /// Tab on an argument, like a shell: complete as far as it is unambiguous, and list the
    /// choices if several remain. Then the arrows choose one, and Tab or Enter put it in.
    pub(super) fn complete_argument(&mut self) {
        self.candidates.clear();
        let Some((kind, head, partial)) = self.completion_context() else {
            return;
        };

        // What the completion is, and what stands before and after a chosen match.
        let (completion, base, suffix) = match kind {
            ArgKind::Path => {
                // A match is a file name; the directory in front of it stays.
                let directory_end = partial.rfind('/').map_or(0, |slash| slash + 1);
                let base = format!("{head}{}", &partial[..directory_end]);
                (completion::complete_path(&partial), base, String::new())
            }
            ArgKind::Session => {
                let names = session::list().unwrap_or_default();
                (
                    completion::complete_from(&partial, &names),
                    head.clone(),
                    String::new(),
                )
            }
            ArgKind::Ssh => {
                // `partial` is `ssh:` + `[user@]host`. A chosen host is followed by the colon
                // that starts the path.
                let rest = partial.strip_prefix("ssh:").unwrap_or(&partial);
                let user_end = rest.rfind('@').map_or(0, |at| at + 1);
                let base = format!("{head}ssh:{}", &rest[..user_end]);
                let hosts = completion::ssh_hosts();
                let completed = completion::complete_ssh(rest, &hosts).map(|mut done| {
                    done.text = format!("ssh:{}", done.text);
                    done
                });
                (completed, base, ":".to_string())
            }
        };

        if let Some(done) = completion {
            self.input.set(&format!("{head}{}", done.text));
            if done.candidates.len() > 1 {
                let mut items = done.candidates;
                items.truncate(MAX_CANDIDATES);
                self.candidates = Candidates {
                    items,
                    selected: 0,
                    base,
                    suffix,
                };
            }
        }
    }

    /// Puts the highlighted match into the prompt and closes the box.
    pub(super) fn insert_candidate(&mut self) {
        if let Some(text) = self.candidates.chosen_text() {
            self.input.set(&text);
        }
        self.candidates.clear();
    }
}
