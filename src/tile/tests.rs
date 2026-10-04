use super::title::{MAX_WHY, shorten};
use super::*;
use crate::highlight::Highlight;
use crate::timestamp;
use crate::when::When;
use std::time::{Duration, Instant};

fn wait_for_lines(tile: &mut Tile, count: u64) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while tile.total_lines() < count {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {count} lines"
        );
        tile.pump();
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn numbered_file(tag: &str, lines: u32) -> (String, Vec<u64>) {
    let path = std::env::temp_dir().join(format!("tilog-tile-{tag}-{}.log", std::process::id()));
    let mut content = String::new();
    let mut offsets = Vec::new();
    for i in 0..lines {
        offsets.push(content.len() as u64);
        content.push_str(&format!("line {i:05}\n"));
    }
    std::fs::write(&path, content).unwrap();
    (path.to_str().unwrap().to_string(), offsets)
}

#[test]
fn showing_an_offset_far_back_loads_a_history_window_and_g_returns() {
    let (path, offsets) = numbered_file("window", 3000);
    let mut tile = Tile::source(&path, 100).unwrap(); // holds lines 2900..3000
    wait_for_lines(&mut tile, 100);

    assert!(tile.show_offset(offsets[50], 1, 20));
    assert!(tile.is_history_view());
    assert!(!tile.is_following());
    let rows = tile.visible(20);
    assert!(
        rows.contains(&"line 00050".to_string()),
        "the match is on screen: {rows:?}"
    );
    assert!(rows[0].as_str() < "line 00050", "with context above it");
    let marked = tile.marked_rows().unwrap();
    assert_eq!(
        tile.text_between(marked.start, marked.end - 1),
        ["line 00050"]
    );

    // Scrolling past the loaded stretch reads on from disk.
    tile.scroll_down(20, 500);
    assert!(tile.visible(20)[0].as_str() > "line 00500");
    // And scrolling up reads what is before it.
    tile.jump_to_start();
    tile.scroll_up(20, 5);
    assert!(tile.visible(20)[0].as_str() < "line 00050");

    tile.jump_to_end();
    assert!(!tile.is_history_view() && tile.is_following());
    assert!(tile.marked_rows().is_none());
    assert_eq!(tile.visible(20).last().unwrap(), "line 02999");
    std::fs::remove_file(&path).unwrap();
}

#[test]
fn showing_an_offset_that_is_loaded_needs_no_window() {
    let (path, offsets) = numbered_file("live", 300);
    let mut tile = Tile::source(&path, 100).unwrap();
    wait_for_lines(&mut tile, 100);

    assert!(tile.show_offset(offsets[250], 2, 20));
    assert!(!tile.is_history_view());
    let marked = tile.marked_rows().unwrap();
    assert_eq!(
        tile.text_between(marked.start, marked.end - 1),
        ["line 00250", "line 00251"]
    );
    assert!(tile.visible(20).contains(&"line 00250".to_string()));
    std::fs::remove_file(&path).unwrap();
}

#[test]
fn stepping_through_a_filter_reports_each_match_with_its_place() {
    let (path, offsets) = numbered_file("step", 500);
    let filter = Filter::new(None, "line 00(100|300)", true, false).unwrap();
    let mut tile = Tile::filtered(&path, filter, GroupRule::default()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while tile.matches() != Some(2) {
        assert!(Instant::now() < deadline, "matches never arrived");
        tile.pump();
        std::thread::sleep(Duration::from_millis(10));
    }

    let first = tile.step_match(true, 10).unwrap();
    assert_eq!(
        (first.at, first.index, first.total),
        (Link::Offset(offsets[100]), 0, 2)
    );
    let second = tile.step_match(true, 10).unwrap();
    assert_eq!(second.at, Link::Offset(offsets[300]));
    let wrapped = tile.step_match(true, 10).unwrap();
    assert!(wrapped.wrapped && wrapped.index == 0);
    assert!(tile.marked_rows().is_some() && tile.title("").contains("1/2"));
    std::fs::remove_file(&path).unwrap();
}

#[test]
fn opens_at_the_end_and_loads_older_lines_when_scrolling_up() {
    let content: String = (0..1000).map(|i| format!("line {i:04}\n")).collect();
    let path = std::env::temp_dir().join(format!("tilog-tile-test-{}.log", std::process::id()));
    std::fs::write(&path, &content).unwrap();

    // Only the last 100 lines are loaded at startup.
    let mut tile = Tile::source(path.to_str().unwrap(), 100).unwrap();
    wait_for_lines(&mut tile, 100);
    assert_eq!(tile.visible(3), ["line 0997", "line 0998", "line 0999"]);
    assert!(tile.title("x").contains("more above"));

    // Scrolling up past what is loaded fetches older lines from disk.
    tile.scroll_up(3, 150);
    assert_eq!(tile.visible(3)[0], "line 0847");
    assert!(!tile.view.is_following());

    // Everything older fits in the history allowance, so nothing is "above" any more.
    assert!(!tile.title("x").contains("more above"));
    tile.scroll_up(3, 5000);
    assert_eq!(tile.visible(1), ["line 0000"]);

    // Following again releases the history: back to the live size.
    tile.jump_to_end();
    tile.pump();
    assert_eq!(tile.visible(1), ["line 0999"]);
    assert!(tile.title("x").contains("more above"));

    std::fs::remove_file(&path).unwrap();
}

#[test]
fn searching_moves_to_the_next_and_previous_match() {
    let content: String = (0..100).map(|i| format!("line {i:03}\n")).collect();
    let path = std::env::temp_dir().join(format!("tilog-tile-find-{}.log", std::process::id()));
    std::fs::write(&path, &content).unwrap();
    let mut tile = Tile::source(path.to_str().unwrap(), 1000).unwrap();
    wait_for_lines(&mut tile, 100);

    let pattern = Highlight::literal("line 05"); // matches lines 050 to 059
    let at = |tile: &Tile| {
        tile.match_row
            .map(|row| row - tile.content.lines().first_seq())
    };

    // Following the end of the log, the first search finds the newest match.
    assert_eq!(tile.find_match(&pattern, 6, Find::First), Some(false));
    assert_eq!(at(&tile), Some(59));
    // `n` continues downwards: nothing below, so it wraps around to the first one.
    assert_eq!(tile.find_match(&pattern, 6, Find::Next), Some(true));
    assert_eq!(at(&tile), Some(50));
    assert_eq!(tile.find_match(&pattern, 6, Find::Next), Some(false));
    assert_eq!(at(&tile), Some(51));
    // `N` goes back up, and wraps from the first match to the last.
    assert_eq!(tile.find_match(&pattern, 6, Find::Previous), Some(false));
    assert_eq!(at(&tile), Some(50));
    assert_eq!(tile.find_match(&pattern, 6, Find::Previous), Some(true));
    assert_eq!(at(&tile), Some(59));

    // Not following: the first search looks down from the top of the view.
    tile.jump_to_start();
    assert_eq!(tile.find_match(&pattern, 6, Find::First), Some(false));
    assert_eq!(at(&tile), Some(50));
    assert!(tile.visible(6).contains(&"line 050".to_string()));

    assert_eq!(
        tile.find_match(&Highlight::literal("absent"), 6, Find::First),
        None
    );
    assert_eq!(
        tile.find_match(&Highlight::literal("absent"), 6, Find::Previous),
        None
    );
    std::fs::remove_file(&path).unwrap();
}

#[test]
fn scrolling_a_log_shorter_than_its_window_does_not_panic() {
    // The reported crash: scrolling up in a tile with fewer lines than rows.
    let path = std::env::temp_dir().join(format!("tilog-tile-short-{}.log", std::process::id()));
    std::fs::write(&path, "a\nb\nc\n").unwrap();
    let mut tile = Tile::source(path.to_str().unwrap(), 100).unwrap();
    wait_for_lines(&mut tile, 3);

    for _ in 0..3 {
        tile.scroll_up(20, 1);
        tile.scroll_up(20, 50);
        tile.scroll_down(20, 50);
        tile.jump_to_start();
    }
    assert_eq!(tile.visible(20), ["a", "b", "c"]);
    assert_eq!(
        tile.vertical_extent(20),
        None,
        "everything fits: no scrollbar"
    );
    std::fs::remove_file(&path).unwrap();
}

#[test]
fn long_error_messages_are_cut_for_the_title() {
    assert_eq!(shorten("exit 1", 44), "exit 1");
    let long = "exit 255: ssh: Could not resolve hostname prod-1: nodename nor servname provided";
    let short = shorten(long, MAX_WHY);
    assert_eq!(short.chars().count(), MAX_WHY);
    assert!(short.starts_with("exit 255: ssh: Could not resolve") && short.ends_with('…'));
}

#[test]
fn a_small_file_is_loaded_completely() {
    let path = std::env::temp_dir().join(format!("tilog-tile-small-{}.log", std::process::id()));
    std::fs::write(&path, "a\nb\n").unwrap();
    let mut tile = Tile::source(path.to_str().unwrap(), 100).unwrap();
    wait_for_lines(&mut tile, 2);
    assert_eq!(tile.visible(10), ["a", "b"]);
    assert!(!tile.title("x").contains("more above"));
    tile.visible(1); // drawn once at this height, as on screen
    tile.jump_to_line(2);
    assert_eq!(tile.visible(1), ["b"]);
    std::fs::remove_file(&path).unwrap();
}

#[test]
fn error_keys_jump_between_error_lines_and_mark_them() {
    let path = std::env::temp_dir().join(format!("tilog-tile-errors-{}.log", std::process::id()));
    let content: String = (0..300)
        .map(|i| match i {
            50 | 150 | 250 => format!("2026-10-03 08:00:00 ERROR boom {i}\n"),
            _ => format!("2026-10-03 08:00:00 INFO fine {i}\n"),
        })
        .collect();
    std::fs::write(&path, content).unwrap();
    let mut tile = Tile::source(path.to_str().unwrap(), 1000).unwrap();
    wait_for_lines(&mut tile, 300);

    // Following the end, `[` finds the newest error first, then the one before it.
    assert_eq!(tile.find_error(false, 10), Some(false));
    assert!(tile.visible(10).iter().any(|row| row.ends_with("boom 250")));
    let marked = tile.marked_rows().unwrap();
    assert_eq!(
        tile.text_between(marked.start, marked.end - 1),
        ["2026-10-03 08:00:00 ERROR boom 250"]
    );
    assert_eq!(tile.find_error(false, 10), Some(false));
    assert!(tile.visible(10).iter().any(|row| row.ends_with("boom 150")));
    // `]` goes forward again, and wraps past the last one.
    assert_eq!(tile.find_error(true, 10), Some(false));
    assert!(tile.visible(10).iter().any(|row| row.ends_with("boom 250")));
    assert_eq!(tile.find_error(true, 10), Some(true));
    assert!(tile.visible(10).iter().any(|row| row.ends_with("boom 50")));
    std::fs::remove_file(&path).unwrap();
}

#[test]
fn pausing_freezes_the_view_where_it_is() {
    let (path, _) = numbered_file("pause", 200);
    let mut tile = Tile::source(&path, 1000).unwrap();
    wait_for_lines(&mut tile, 200);
    let rows = tile.visible(10);
    assert!(tile.is_following());

    tile.pause();
    assert!(!tile.is_following());
    assert_eq!(tile.visible(10), rows, "nothing moved");
    std::fs::remove_file(&path).unwrap();
}

fn timed_file(tag: &str, lines: i64) -> String {
    let path = std::env::temp_dir().join(format!("tilog-tile-{tag}-{}.log", std::process::id()));
    let content: String = (0..lines)
        .map(|i| {
            let (m, s) = (i / 60, i % 60);
            format!("2026-10-03 08:{m:02}:{s:02} INFO line {i}\n")
        })
        .collect();
    std::fs::write(&path, content).unwrap();
    path.to_str().unwrap().to_string()
}

#[test]
fn goto_time_finds_a_line_outside_the_loaded_ones_and_the_nearest_after() {
    let path = timed_file("goto", 3000); // 08:00:00 .. 08:49:59, only the newest 100 loaded
    let mut tile = Tile::source(&path, 100).unwrap();
    wait_for_lines(&mut tile, 100);
    tile.visible(10); // drawn once, so the tile knows its height

    let landed = tile
        .jump_to_time(&When::parse("08:10:30").unwrap())
        .unwrap();
    assert_eq!(
        landed,
        super::time::Landed {
            time: timestamp::parse("2026-10-03 08:10:30", 0).unwrap(),
            past_end: false
        }
    );
    assert!(tile.is_history_view());
    assert!(
        tile.visible(10)
            .iter()
            .any(|row| row.contains("08:10:30 INFO line 630"))
    );
    let marked = tile.marked_rows().unwrap();
    assert!(tile.text_between(marked.start, marked.end - 1)[0].contains("08:10:30"));

    // A time that no line has lands on the next one.
    std::fs::write(
        &path,
        "2026-10-03 08:00:00 a\n2026-10-03 08:00:10 b\n2026-10-03 08:00:20 c\n",
    )
    .unwrap();
    let mut small = Tile::source(&path, 100).unwrap();
    wait_for_lines(&mut small, 3);
    small.visible(10);
    let landed = small
        .jump_to_time(&When::parse("08:00:05").unwrap())
        .unwrap();
    assert_eq!(timestamp::utc_clock(landed.time), "08:00:10");
    // After the end: the last line with a time, and it says so.
    let landed = small
        .jump_to_time(&When::parse("2026-10-03 09:00").unwrap())
        .unwrap();
    assert!(landed.past_end);
    assert_eq!(timestamp::utc_clock(landed.time), "08:00:20");
    std::fs::remove_file(&path).unwrap();
}

#[test]
fn goto_time_works_in_a_filter_and_says_when_there_are_no_times() {
    let path = timed_file("gotofilter", 600);
    let filter = Filter::new(None, "line 3", false, false).unwrap(); // lines 3, 30-39, 300-399...
    let mut tile = Tile::filtered(&path, filter, GroupRule::default()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while tile.matches().is_none_or(|n| n < 111) {
        assert!(Instant::now() < deadline, "matches never arrived");
        tile.pump();
        std::thread::sleep(Duration::from_millis(10));
    }
    tile.visible(10);
    // 08:05:00 is line 300: the first matching entry at or after it.
    let landed = tile
        .jump_to_time(&When::parse("08:05:00").unwrap())
        .unwrap();
    assert_eq!(timestamp::utc_clock(landed.time), "08:05:00");

    let mut stream = Tile::stream_filtered(
        Filter::new(None, "x", false, false).unwrap(),
        GroupRule::default(),
        &["no time here x".to_string()],
        0,
        0,
    );
    assert_eq!(
        stream.jump_to_time(&When::parse("08:00").unwrap()),
        Err("no timestamps in this tile".to_string())
    );
    std::fs::remove_file(&path).unwrap();
}

#[test]
fn write_saves_all_matches_of_a_filter_and_never_overwrites_without_force() {
    let (path, _) = numbered_file("write", 5000);
    let filter = Filter::new(None, "line 004", false, false).unwrap(); // 004xx: 100 matches
    let mut tile = Tile::filtered(&path, filter, GroupRule::default()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while tile.matches() != Some(100) {
        assert!(Instant::now() < deadline, "matches never arrived");
        tile.pump();
        std::thread::sleep(Duration::from_millis(10));
    }

    let out = std::env::temp_dir().join(format!("tilog-write-{}.txt", std::process::id()));
    let _ = std::fs::remove_file(&out);
    assert_eq!(tile.write_to(&out, false).unwrap(), 100);
    let text = std::fs::read_to_string(&out).unwrap();
    assert_eq!(text.lines().count(), 100);
    assert!(text.starts_with("line 00400\n") && text.ends_with("line 00499\n"));

    let again = tile.write_to(&out, false).unwrap_err();
    assert_eq!(again.kind(), std::io::ErrorKind::AlreadyExists);
    assert_eq!(
        std::fs::read_to_string(&out).unwrap(),
        text,
        "the file was left alone"
    );
    assert_eq!(tile.write_to(&out, true).unwrap(), 100);

    // The main tile of a big file holds only its end; a small one holds all of it.
    let mut main = Tile::source(&path, 100).unwrap();
    wait_for_lines(&mut main, 100);
    assert!(!main.holds_whole_log());
    std::fs::remove_file(&out).unwrap();
    std::fs::remove_file(&path).unwrap();
}

#[test]
fn error_keys_and_search_keys_continue_from_their_own_position() {
    let path = std::env::temp_dir().join(format!("tilog-tile-own-{}.log", std::process::id()));
    let content: String = (0..300)
        .map(|i| match i {
            50 | 250 => format!("2026-10-03 08:00:00 ERROR boom {i}\n"),
            100 | 200 => format!("2026-10-03 08:00:00 INFO needle {i}\n"),
            _ => format!("2026-10-03 08:00:00 INFO fine {i}\n"),
        })
        .collect();
    std::fs::write(&path, content).unwrap();
    let mut tile = Tile::source(path.to_str().unwrap(), 1000).unwrap();
    wait_for_lines(&mut tile, 300);
    let needle = Highlight::literal("needle");
    let shows = |tile: &Tile, text: &str| tile.visible(10).iter().any(|row| row.ends_with(text));

    tile.find_match(&needle, 10, Find::First); // the newest: needle 200
    assert!(shows(&tile, "needle 200"));
    let search_row = tile.match_row();

    // Stepping through errors leaves the search's position alone ...
    tile.find_error(true, 10);
    assert!(shows(&tile, "boom 250"));
    assert_eq!(tile.match_row(), search_row);
    // ... and continues from the error it is at, not from the search.
    tile.find_error(false, 10);
    assert!(shows(&tile, "boom 50"));
    assert_eq!(tile.match_row(), search_row);

    // `n` is still a search: it never lands on an error line.
    tile.find_match(&needle, 10, Find::Next);
    assert!(shows(&tile, "needle 100"), "{:?}", tile.visible(10));
    assert!(
        tile.marked_rows().is_none(),
        "the error marker gives way to the search"
    );
    std::fs::remove_file(&path).unwrap();
}

#[test]
fn goto_line_marks_the_row_with_context_above_it() {
    let (path, _) = numbered_file("gotoline", 100);
    let mut tile = Tile::source(&path, 1000).unwrap();
    wait_for_lines(&mut tile, 100);
    tile.visible(12);

    tile.jump_to_line(40);
    let rows = tile.visible(12);
    assert!(
        rows.contains(&"line 00039".to_string()),
        "line 40 is the 40th: {rows:?}"
    );
    assert!(rows[0].as_str() < "line 00039", "context above it");
    let marked = tile.marked_rows().unwrap();
    assert_eq!(
        tile.text_between(marked.start, marked.end - 1),
        ["line 00039"]
    );

    // Past the end: nothing to mark, the view just goes to the end.
    tile.jump_to_line(10_000);
    assert!(tile.marked_rows().is_none());
    assert_eq!(tile.visible(12).last().unwrap(), "line 00099");
    std::fs::remove_file(&path).unwrap();
}

#[test]
fn a_stream_filter_knows_where_its_entries_are_in_the_source() {
    // A stream's main tile holds lines 1000.. (older ones were evicted); the filter is made
    // from them and then fed more.
    let backfill: Vec<String> = (0..6)
        .map(|i| {
            if i % 3 == 0 {
                format!("hit {i}")
            } else {
                format!("other {i}")
            }
        })
        .collect();
    let filter = Filter::new(None, "hit", false, false).unwrap();
    let mut tile = Tile::stream_filtered(filter, GroupRule::Off, &backfill, 1000, 0);
    tile.feed(&["other 6".to_string(), "hit 7".to_string()]);
    tile.pump(); // the last entry is released when nothing follows it
    std::thread::sleep(Duration::from_millis(200));
    tile.pump();
    tile.visible(10);

    let first = tile.step_match(true, 10).unwrap();
    assert_eq!((first.at, first.rows, first.index), (Link::Seq(1000), 1, 0));
    let second = tile.step_match(true, 10).unwrap();
    assert_eq!(second.at, Link::Seq(1003));
    let third = tile.step_match(true, 10).unwrap();
    assert_eq!(
        third.at,
        Link::Seq(1007),
        "fed after the backfill, numbered on from it"
    );
    assert_eq!(third.total, 3);
    assert!(tile.step_match(true, 10).unwrap().wrapped);
    assert!(tile.title("").contains("1/3"));
}

#[test]
fn the_main_tile_of_a_stream_shows_a_seq_and_says_when_it_is_gone() {
    let mut source = Tile::stream(
        match crate::spec::SourceSpec::parse("cmd:true").unwrap() {
            crate::spec::SourceSpec::Command(command) => command,
            _ => unreachable!(),
        },
        1000,
    );
    // Nothing is held yet: a line that is not there can't be shown.
    assert!(!source.show_link(Link::Seq(5), 1, 10));
    assert!(
        !source.show_link(Link::Offset(0), 1, 10),
        "a command has no file to read"
    );
}
#[test]
fn a_huge_line_is_cut_and_the_lines_after_it_are_still_found() {
    let path = std::env::temp_dir().join(format!("tilog-tile-huge-{}.log", std::process::id()));
    let mut content = String::from("first\n");
    content.push_str(&"x".repeat(20_000_000)); // one 20 MB line
    content.push_str("\nlast\n");
    std::fs::write(&path, &content).unwrap();
    let mut tile = Tile::source(path.to_str().unwrap(), 100).unwrap();
    wait_for_lines(&mut tile, 3);

    let rows = tile.visible(10);
    assert_eq!(rows[0], "first");
    assert!(
        rows[1].starts_with("xxxx") && rows[1].ends_with("bytes cut]"),
        "cut: {}",
        rows[1].len()
    );
    assert!(rows[1].len() < 70_000);
    assert_eq!(rows[2], "last");
    std::fs::remove_file(&path).unwrap();
}
