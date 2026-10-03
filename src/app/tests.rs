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
