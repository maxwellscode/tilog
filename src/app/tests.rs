use super::*;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// `count` small log files in a fresh temp directory.
fn logs(tag: &str, count: usize) -> (std::path::PathBuf, Vec<String>) {
    let dir = std::env::temp_dir().join(format!("tilog-app-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let paths = (0..count)
        .map(|i| {
            let path = dir.join(format!("log{i}.log"));
            std::fs::write(&path, format!("2026-10-03 08:00:0{i} line from {i}\n")).unwrap();
            path.to_string_lossy().into_owned()
        })
        .collect();
    (dir, paths)
}

fn error_text(app: &App) -> String {
    match &app.notice {
        Some(notice) if notice.is_error => notice.text.clone(),
        _ => String::new(),
    }
}

fn info_text(app: &App) -> String {
    match &app.notice {
        Some(notice) if !notice.is_error => notice.text.clone(),
        _ => String::new(),
    }
}

#[test]
fn the_same_log_is_not_opened_twice() {
    let (dir, paths) = logs("same", 2);
    let mut app = App::new();
    assert_eq!(app.add_path(&paths[0]).unwrap(), Added::New);
    assert_eq!(app.add_path(&paths[1]).unwrap(), Added::New);

    // The same file, written another way: with a detour through `.`, and with `..`.
    let detour = paths[0].replace("/log0.log", "/./log0.log");
    let up_and_back = paths[0].replace(
        "/log0.log",
        &format!(
            "/../{}/log0.log",
            dir.file_name().unwrap().to_string_lossy()
        ),
    );
    for again in [&paths[0], &detour, &up_and_back] {
        assert_eq!(app.add_path(again).unwrap(), Added::Existing(0), "{again}");
    }
    assert_eq!(app.sources.len(), 2);
    assert_eq!(app.selected, 0, "the one that is open gets selected");

    // A symlink to it is the same file too.
    let link = dir.join("link.log");
    std::os::unix::fs::symlink(&paths[1], &link).unwrap();
    assert_eq!(
        app.add_path(link.to_str().unwrap()).unwrap(),
        Added::Existing(1)
    );

    // Through the command, with a message that says what happened.
    app.run_command_line(&format!("add {}", paths[1]));
    let message = app
        .notice
        .as_ref()
        .map(|n| n.text.clone())
        .unwrap_or_default();
    assert!(message.contains("already open as tab 2"), "{message}");
    assert_eq!(app.sources.len(), 2);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn the_same_command_is_not_started_twice() {
    let mut app = App::new();
    assert_eq!(app.add_path("cmd:sleep 30").unwrap(), Added::New);
    assert_eq!(
        app.add_path("cmd: sleep 30 ").unwrap(),
        Added::Existing(0),
        "spacing doesn't matter"
    );
    assert_eq!(
        app.add_path("cmd:sleep 31").unwrap(),
        Added::New,
        "a different command is another source"
    );
    assert_eq!(app.sources.len(), 2);

    // ssh by host and path; the same path on another host is another log.
    assert_eq!(app.add_path("ssh:h1:/var/log/a.log").unwrap(), Added::New);
    assert_eq!(
        app.add_path("ssh:h1:/var/log/a.log").unwrap(),
        Added::Existing(2)
    );
    assert_eq!(app.add_path("ssh:h2:/var/log/a.log").unwrap(), Added::New);
}

#[test]
fn a_named_source_is_the_same_as_its_spec() {
    let mut app = App::new();
    app.named_sources.push(NamedSource {
        name: "api".into(),
        spec: "docker:api-container".into(),
    });
    assert_eq!(app.add_path("docker:api-container").unwrap(), Added::New);
    assert_eq!(app.add_path("api").unwrap(), Added::Existing(0));
    assert_eq!(app.sources.len(), 1);
}

#[test]
fn asking_for_an_open_log_is_fine_when_full_but_a_new_one_is_not() {
    let (dir, paths) = logs("full-same", MAX_SOURCES + 1);
    let mut app = App::new();
    for path in &paths[..MAX_SOURCES] {
        app.add_path(path).unwrap();
    }
    assert_eq!(app.add_path(&paths[3]).unwrap(), Added::Existing(3));
    assert!(app.add_path(&paths[MAX_SOURCES]).is_err());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn sources_stop_at_the_limit() {
    let (dir, paths) = logs("limit", MAX_SOURCES + 1);
    let mut app = App::new();
    for path in &paths[..MAX_SOURCES] {
        app.add_path(path).unwrap();
    }
    assert!(app.is_full());

    let err = app.add_path(&paths[MAX_SOURCES]).unwrap_err().to_string();
    assert!(err.contains("at most 9 sources"), "{err}");
    // The same through the command, which reports it instead of failing.
    app.run_command_line(&format!("add {}", paths[MAX_SOURCES]));
    assert!(error_text(&app).contains("at most 9 sources"));
    assert_eq!(app.sources.len(), MAX_SOURCES);

    // Closing one makes room again.
    app.run_command_line("close");
    assert!(!app.is_full());
    app.add_path(&paths[MAX_SOURCES]).unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn the_timeline_is_extra_and_does_not_take_a_source_slot() {
    let (dir, paths) = logs("merge", MAX_SOURCES + 1);
    let mut app = App::new();
    for path in &paths[..MAX_SOURCES] {
        app.add_path(path).unwrap();
    }
    assert!(app.is_full());

    // With all nine slots used, merging still works: the timeline is a tenth tab.
    app.run_command_line("merge");
    assert_eq!(error_text(&app), "");
    assert_eq!(app.sources.len(), MAX_SOURCES + 1);
    assert!(app.sources.last().unwrap().is_merged());
    assert_eq!(app.tab, MAX_SOURCES + 1, "it is opened");

    // It does not make room for a tenth *source*, and merging again replaces it.
    assert!(app.is_full());
    assert!(app.add_path(&paths[MAX_SOURCES]).is_err());
    app.run_command_line("merge");
    assert_eq!(app.sources.iter().filter(|s| s.is_merged()).count(), 1);
    assert_eq!(app.sources.len(), MAX_SOURCES + 1);

    // Closing a real source makes room for a new one, timeline or not.
    app.tab = 0;
    app.selected = 0;
    app.run_command_line("close");
    assert!(!app.is_full());
    app.add_path(&paths[MAX_SOURCES]).unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn the_m_key_jumps_to_the_timeline() {
    let (dir, paths) = logs("mkey", 3);
    let mut app = App::new();
    for path in &paths {
        app.add_path(path).unwrap();
    }
    let areas = app.areas(Rect::new(0, 0, 120, 30));
    let press = |app: &mut App, ch: char| {
        app.on_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE), &areas);
    };

    // No timeline yet: the key says so instead of doing nothing.
    press(&mut app, 'm');
    assert_eq!(app.tab, 0);
    assert!(
        error_text(&app).contains("no merged timeline"),
        "{}",
        error_text(&app)
    );

    app.run_command_line("merge"); // opens the timeline, the 4th tab
    app.tab = 1; // somewhere else
    press(&mut app, 'm');
    assert_eq!(app.tab, 4);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn n_in_a_filter_pane_moves_the_main_pane_to_the_match() {
    let dir = std::env::temp_dir().join(format!("tilog-app-nstep-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("big.log").to_string_lossy().into_owned();
    let content: String = (0..20_000)
        .map(|i| {
            if i == 100 {
                "needle here\n".to_string()
            } else {
                format!("2026-10-03 08:00:00 filler {i}\n")
            }
        })
        .collect();
    std::fs::write(&path, content).unwrap();

    let mut app = App::new();
    app.add_path(&path).unwrap();
    app.tab = 1;
    app.run_command_line("filter needle");
    let areas = app.areas(Rect::new(0, 0, 120, 30));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while app.tile_ref(1).and_then(Tile::matches) != Some(1) {
        assert!(
            std::time::Instant::now() < deadline,
            "the filter found nothing"
        );
        app.pump_sources();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    app.sources[0].set_focus(1);

    let press = |app: &mut App, ch: char| {
        app.on_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE), &areas);
    };
    press(&mut app, 'n');
    let main = app.tile_ref(0).unwrap();
    assert!(
        main.visible(areas.tile_height(0))
            .contains(&"needle here".to_string())
    );
    assert!(
        main.is_history_view(),
        "line 100 is far from the loaded end"
    );

    // G in the main pane returns it to the live end.
    app.sources[0].set_focus(0);
    press(&mut app, 'G');
    assert!(!app.tile_ref(0).unwrap().is_history_view());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn follow_all_resumes_following_in_every_tile_of_every_source() {
    let (dir, paths) = logs("followall", 2);
    let mut app = App::new();
    for path in &paths {
        app.add_path(path).unwrap();
    }
    for tab in 1..=2 {
        app.tab = tab;
        app.run_command_line("filter line");
        app.sources[tab - 1].tile_mut(0).unwrap().jump_to_start();
    }
    assert!(
        app.sources
            .iter()
            .all(|source| !source.main().is_following())
    );

    app.tab = 0;
    app.run_command_line("follow all");
    for source in &app.sources {
        assert!(source.tiles().iter().all(|tile| tile.is_following()));
    }
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn filter_a_opens_the_filter_on_every_source_and_pause_all_freezes_them() {
    let (dir, paths) = logs("filtera", 3);
    let mut app = App::new();
    for path in &paths {
        app.add_path(path).unwrap();
    }
    app.run_command_line("filter -a line");
    for source in &app.sources {
        assert_eq!(source.tiles().len(), 2, "main tile and one filter");
        assert_eq!(
            source.tiles()[1].filter().map(|f| f.label()),
            Some("line".into())
        );
    }

    app.run_command_line("pause all");
    for source in &app.sources {
        assert!(source.tiles().iter().all(|tile| !tile.is_following()));
    }
    app.run_command_line("follow all");
    assert!(
        app.sources
            .iter()
            .all(|source| source.main().is_following())
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn write_command_reports_success_and_refuses_to_overwrite() {
    let (dir, paths) = logs("writecmd", 1);
    let mut app = App::new();
    app.add_path(&paths[0]).unwrap();
    app.tab = 1;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while app.tile_ref(0).is_none_or(|tile| tile.total_lines() < 1) {
        assert!(
            std::time::Instant::now() < deadline,
            "the line never arrived"
        );
        app.pump_sources();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let out = dir.join("out.txt").to_string_lossy().into_owned();

    app.run_command_line(&format!("write {out}"));
    assert_eq!(
        std::fs::read_to_string(&out).unwrap(),
        "2026-10-03 08:00:00 line from 0\n"
    );
    assert!(
        info_text(&app).contains("wrote 1 lines"),
        "{}",
        info_text(&app)
    );

    app.run_command_line(&format!("write {out}"));
    assert!(error_text(&app).contains("exists"), "{}", error_text(&app));
    app.run_command_line(&format!("write -f {out}"));
    assert!(info_text(&app).contains("wrote 1 lines"));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn messages_fade_after_a_while_and_errors_last_longer() {
    let mut app = App::new();
    let start = std::time::Instant::now();
    let after = |seconds| start + std::time::Duration::from_secs(seconds);

    app.info("done");
    app.expire_notice(after(2));
    assert!(app.notice.is_some(), "still there after 2 s");
    app.expire_notice(after(4));
    assert!(app.notice.is_none(), "gone after 3 s");

    app.error("it failed");
    app.expire_notice(after(4));
    assert!(app.notice.is_some(), "an error is still there after 4 s");
    app.expire_notice(after(6));
    assert!(app.notice.is_none(), "but not after 5 s");
}

#[test]
fn a_session_leaves_out_standard_input_and_keeps_the_positions_right() {
    let (dir, paths) = logs("stdinsession", 2);
    let mut app = App::new();
    app.add_path(&paths[0]).unwrap();
    app.sources.push(Source::fake_stdin()); // second in the list
    app.add_path(&paths[1]).unwrap(); // third
    app.run_command_line("merge 1 3");
    assert_eq!(app.sources.len(), 4);

    app.tab = 3; // the log that comes after the pipe
    app.selected = 2;
    let session = app.snapshot();
    let saved: Vec<&str> = session.sources.iter().map(|s| s.path.as_str()).collect();
    assert_eq!(saved.len(), 3, "{saved:?}");
    assert!(!saved.contains(&"-"), "the pipe is not saved");
    assert_eq!(
        saved[2], "merge:0,1",
        "the members are the two logs, counted without the pipe"
    );
    assert_eq!(
        session.tab, 2,
        "tab 3 was the second log, which is now the second tab"
    );
    assert_eq!(session.selected, 1);

    // On the pipe itself there is nothing to come back to: the overview.
    app.tab = 2;
    assert_eq!(app.snapshot().tab, 0);
    std::fs::remove_dir_all(&dir).unwrap();
}

/// A source that is a command whose output is `lines`, with its main tile filled.
fn stream_source(app: &mut App, lines: &[String]) {
    let mut source = Source::fake_stdin();
    source.tile_mut(0).unwrap().push_for_test(lines);
    app.sources.push(source);
}

#[test]
fn n_in_a_filter_of_a_stream_moves_the_main_pane_and_a_search_does_too() {
    let mut app = App::new();
    let lines: Vec<String> = (0..200)
        .map(|i| match i {
            40 => "2026-10-03 08:00:00 INFO needle alpha".to_string(),
            150 => "2026-10-03 08:00:00 INFO needle beta".to_string(),
            _ => format!("2026-10-03 08:00:00 INFO filler {i}"),
        })
        .collect();
    stream_source(&mut app, &lines);
    app.tab = 1;
    app.run_command_line("filter needle");
    let areas = app.areas(Rect::new(0, 0, 120, 30));
    let press = |app: &mut App, ch: char| {
        app.on_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE), &areas);
    };
    let main_shows = |app: &App, text: &str| {
        app.tile_ref(0)
            .unwrap()
            .visible(areas.tile_height(0))
            .iter()
            .any(|row| row.ends_with(text))
    };
    assert_eq!(app.target_index(), 1, "the new filter is focused");

    // Without a search, `n` steps through the filter's entries and the main pane follows.
    press(&mut app, 'n');
    assert!(main_shows(&app, "needle alpha"));
    press(&mut app, 'n');
    assert!(main_shows(&app, "needle beta"));
    press(&mut app, 'N');
    assert!(main_shows(&app, "needle alpha"));

    // With a search on, `n` goes to the next hit in the filter pane, and the main pane follows.
    app.highlight = Some(Highlight::literal("beta"));
    press(&mut app, 'n');
    assert!(main_shows(&app, "needle beta"));
}

#[test]
fn a_search_in_a_file_filter_pane_also_moves_the_main_pane() {
    let dir = std::env::temp_dir().join(format!("tilog-app-fsearch-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("big.log").to_string_lossy().into_owned();
    let content: String = (0..20_000)
        .map(|i| match i {
            100 => "WARN disk almost full on sda1\n".to_string(),
            200 => "WARN disk almost full on sdb1\n".to_string(),
            _ => format!("2026-10-03 08:00:00 INFO filler {i}\n"),
        })
        .collect();
    std::fs::write(&path, content).unwrap();
    let mut app = App::new();
    app.add_path(&path).unwrap();
    app.tab = 1;
    app.run_command_line("filter WARN");
    let areas = app.areas(Rect::new(0, 0, 120, 30));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while app.tile_ref(1).and_then(Tile::matches) != Some(2) {
        assert!(
            std::time::Instant::now() < deadline,
            "the filter found nothing"
        );
        app.pump_sources();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    app.highlight = Some(Highlight::literal("sdb1"));
    app.on_key(
        KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE),
        &areas,
    );

    let main = app.tile_ref(0).unwrap();
    assert!(main.is_history_view());
    assert!(
        main.visible(areas.tile_height(0))
            .iter()
            .any(|row| row.ends_with("sdb1"))
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn picking_a_command_with_optional_arguments_from_the_menu_waits_for_them() {
    let mut app = App::new();
    app.add_path(&logs("menuwait", 1).1[0]).unwrap();
    app.tab = 1;
    let areas = app.areas(Rect::new(0, 0, 120, 30));
    let key = |app: &mut App, code: KeyCode| {
        app.on_key(KeyEvent::new(code, KeyModifiers::NONE), &areas);
    };
    let type_text = |app: &mut App, text: &str| {
        for ch in text.chars() {
            key(app, KeyCode::Char(ch));
        }
    };

    // `follow [all]`: Enter on the menu entry puts the command in the prompt and waits, so
    // `all` can still be typed.
    key(&mut app, KeyCode::Char(':'));
    type_text(&mut app, "follow");
    key(&mut app, KeyCode::Enter);
    assert!(app.prompt.is_some(), "the prompt is still open");
    assert_eq!(app.input.text(), "follow ");
    assert!(app.notice.is_none(), "nothing ran yet");
    type_text(&mut app, "all");
    key(&mut app, KeyCode::Enter);
    assert!(app.prompt.is_none());
    assert!(
        info_text(&app).contains("following everywhere"),
        "{}",
        info_text(&app)
    );

    // A command without arguments still runs at once.
    key(&mut app, KeyCode::Char(':'));
    type_text(&mut app, "top");
    key(&mut app, KeyCode::Enter);
    assert!(app.prompt.is_none());
    assert!(
        info_text(&app).contains("first line"),
        "{}",
        info_text(&app)
    );
}

#[test]
fn typing_answers_the_question_of_the_selected_tile_on_the_overview() {
    use crate::askpass::{Ask, Kind};
    let mut app = App::new();
    let (_dir, paths) = logs("askkeys", 1);
    app.add_path(&paths[0]).unwrap();
    app.sources.push(Source::fake_stdin());
    app.selected = 1;
    let (ask, answers) = Ask::for_test("deploy@web1's password: ", Kind::Secret);
    app.sources[1].tile_mut(0).unwrap().ask_for_test(ask);
    let areas = app.areas(Rect::new(0, 0, 120, 30));
    let key = |app: &mut App, code: KeyCode| {
        app.on_key(KeyEvent::new(code, KeyModifiers::NONE), &areas);
    };

    // The keys go to the question, even the ones that mean something otherwise.
    assert!(
        app.hint().contains("asks: type the answer"),
        "{}",
        app.hint()
    );
    for c in "q:1&?".chars() {
        key(&mut app, KeyCode::Char(c));
    }
    key(&mut app, KeyCode::Backspace);
    assert!(!app.quit && app.tab == 0 && app.prompt.is_none() && !app.show_help);
    key(&mut app, KeyCode::Enter);
    assert_eq!(answers.recv().unwrap().as_deref(), Some("q:1&"));

    // Esc gives up the next question.
    let (ask, answers) = Ask::for_test("password: ", Kind::Secret);
    app.sources[1].tile_mut(0).unwrap().ask_for_test(ask);
    key(&mut app, KeyCode::Esc);
    assert_eq!(answers.recv().unwrap(), None);

    // A question in a tile that is not selected is left alone: Tab chooses between them.
    let (ask, answers) = Ask::for_test("password: ", Kind::Secret);
    app.sources[1].tile_mut(0).unwrap().ask_for_test(ask);
    key(&mut app, KeyCode::Tab);
    assert_eq!(app.selected, 0, "Tab still moves the selection");
    key(&mut app, KeyCode::Char('x'));
    assert!(
        answers.try_recv().is_err(),
        "x was not typed into the other tile"
    );
    assert!(app.sources[1].is_asking());
}
