use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use ordet_format::{read_from_path, write_to_path, OrdetDocument};

use super::cli::{parse_cli_args, CliAction};
use super::{
    load_buffer_from_path, load_plain_text_from_path, parse_alignment_input,
    parse_browser_target_input, parse_pasted_single_path, parse_word_target,
    suggest_copy_destination, unique_trash_destination, OrdetApp,
};
use crate::commands::Command;
use crate::editor_buffer::{EditorBuffer, ParagraphAlignment};
use crate::state::PanelKind;
use crate::state::{AppState, HelpTab};
use crate::theme::ThemeSelection;
use ordet_tui_framework::{Rect, UiEvent};

#[test]
fn esc_leaves_welcome_help_on_the_first_press() {
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    let mut app =
        with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
    assert!(app.state.show_welcome_screen());
    app.state
        .set_welcome_selection(crate::state::WelcomeOption::Help);
    app.state.focus_welcome_help();
    assert!(app.state.welcome_help_active());

    // The app-level Esc handling must not swallow this first: on the
    // welcome screen Esc belongs to the help overlay.
    let esc = KeyEvent {
        code: KeyCode::Esc,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::empty(),
    };
    app.handle_key_event(esc);
    assert!(!app.state.welcome_help_active());
    assert_eq!(
        app.state.ui.welcome_selected,
        crate::state::WelcomeOption::Help
    );
}

#[test]
fn word_target_prompt_accepts_numbers_and_clear_commands() {
    assert_eq!(parse_word_target("1,500"), Ok(Some(1500)));
    assert_eq!(parse_word_target("off"), Ok(None));
    assert!(parse_word_target("nope").is_err());
}

#[test]
fn empty_ord_files_open_as_blank_documents() {
    let path = unique_temp_path("ordet-empty", "ord");
    fs::write(&path, "").expect("empty ord file");

    let text = load_plain_text_from_path(&path).expect("open empty ord");

    assert!(text.is_empty());
}

#[test]
fn alignment_prompt_accepts_named_values() {
    assert_eq!(parse_alignment_input("left"), Ok(ParagraphAlignment::Left));
    assert_eq!(
        parse_alignment_input("center"),
        Ok(ParagraphAlignment::Center)
    );
    assert_eq!(
        parse_alignment_input("right"),
        Ok(ParagraphAlignment::Right)
    );
    assert_eq!(
        parse_alignment_input("justify"),
        Ok(ParagraphAlignment::Justify)
    );
    assert!(parse_alignment_input("diagonal").is_err());
}

#[test]
fn missing_startup_path_opens_unsaved_draft_instead_of_binding_file() {
    let path = unique_temp_path("ordet-missing-startup", "ord");
    if path.exists() {
        fs::remove_file(&path).expect("remove stale temp file");
    }

    let app = with_test_settings(|| {
        OrdetApp::from_args(vec![path.to_string_lossy().into_owned()])
            .expect("app from missing arg path")
    });

    assert!(app.state.editor().file_path.is_none());
    assert!(
        app.state.ui.status.contains("did not exist"),
        "status was {:?}",
        app.state.ui.status
    );
}

#[test]
fn startup_without_args_keeps_the_auto_scratch_invisible() {
    let app = with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app from empty args"));

    // The auto-scratch draft is a private landing pad: the page list
    // shows none until the user types or opens something.
    assert_eq!(app.state.visible_draft_count(), 0);
    assert!(app.state.page_rows().is_empty());
}

#[test]
fn idle_loop_waits_for_the_next_housekeeping_deadline() {
    let mut app =
        with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app from empty args"));
    let now = std::time::Instant::now();
    app.next_tick = now + std::time::Duration::from_millis(100);
    app.redraw_requested = false;
    app.next_recovery_write = now + std::time::Duration::from_secs(3);
    app.next_autosave_at = now + std::time::Duration::from_secs(2);

    let (deadlines, count) = app.next_deadlines();
    assert_eq!(count, 3);
    let wait = deadlines
        .iter()
        .take(count)
        .map(|at| at.saturating_duration_since(now))
        .min()
        .expect("deadlines");
    assert!(wait >= std::time::Duration::from_millis(90));
}

#[test]
fn startup_positions_cursor_at_end_for_existing_files() {
    let path = unique_temp_path("ordet-startup-cursor", "txt");
    fs::write(&path, "hello world").expect("startup text file");

    let app = with_test_settings(|| {
        OrdetApp::from_args(vec![path.to_string_lossy().into_owned()])
            .expect("app from startup path")
    });

    assert_eq!(
        app.state.editor().buffer.cursor(),
        app.state.editor().buffer.char_count()
    );
}

#[test]
fn saving_over_a_directory_reports_an_error_instead_of_panicking() {
    let dir = unique_temp_path("ordet-save-dir", "");
    fs::create_dir_all(&dir).expect("directory target");

    let result = OrdetApp::write_draft_file(&dir, "text", "", "title", "Draft", true, "");

    assert!(result.is_err(), "a directory is not a writable file target");
}

#[test]
fn binary_file_is_rejected_without_panicking() {
    let path = unique_temp_path("ordet-binary", "bin");
    fs::write(&path, [0xff, 0xfe, 0x00, 0x80]).expect("binary fixture");

    let error = load_buffer_from_path(&path).expect_err("binary file must not open as text");
    assert!(
        !error.to_string().is_empty(),
        "the load error should be readable"
    );
}

#[test]
fn opening_an_already_open_file_focuses_instead_of_duplicating() {
    let path = unique_temp_path("ordet-open-twice", "txt");
    fs::write(&path, "one draft only").expect("fixture");
    let mut app = with_test_settings(|| {
        OrdetApp::from_args(vec![path.to_string_lossy().into_owned()]).expect("app from path")
    });
    let before = app.state.drafts.len();

    app.open_or_focus_path(path.clone());
    app.open_or_focus_path(path.clone());

    assert_eq!(app.state.drafts.len(), before);
    assert_eq!(app.state.window_count(), 1);
    assert_eq!(
        app.state.editor().title(),
        path.file_name().unwrap().to_string_lossy()
    );
}

#[test]
fn f6_cycles_four_windows_forward_and_back() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let mut app =
        with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
    for title in ["one", "two", "three", "four"] {
        app.state.add_draft(EditorBuffer::from_text(title), None);
    }
    app.state.update_layout(Rect::new(0, 0, 120, 30));
    for _ in 0..3 {
        app.handle_key_event(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::ALT));
    }
    assert_eq!(app.state.window_count(), 4);

    for expected in [0, 1, 2, 3, 0] {
        app.handle_key_event(KeyEvent::new(KeyCode::F(6), KeyModifiers::NONE));
        assert_eq!(app.state.active_window_position(), expected);
    }
    for expected in [3, 2, 1, 0, 3] {
        app.handle_key_event(KeyEvent::new(KeyCode::F(6), KeyModifiers::SHIFT));
        assert_eq!(app.state.active_window_position(), expected);
    }
}

#[test]
fn invalid_regex_reopens_find_without_dirtying_the_draft() {
    let mut app =
        with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
    app.state
        .add_draft(EditorBuffer::from_text("hello world"), None);
    app.state.ui.search_options.regex = true;
    let revision = app.state.editor().revision;

    app.run_find("[".to_string());

    let prompt = app.state.ui.prompt.as_ref().expect("invalid regex retries");
    assert!(
        prompt.hint.to_ascii_lowercase().contains("regex"),
        "unexpected hint: {}",
        prompt.hint
    );
    assert_eq!(app.state.editor().revision, revision);
}

#[test]
fn unique_trash_destination_avoids_name_collisions() {
    let trash = unique_temp_path("ordet-trash-dir", "tmp");
    fs::create_dir_all(&trash).expect("trash dir");
    fs::write(trash.join("draft.ord"), "older").expect("existing trash file");

    let destination = unique_trash_destination(&trash, std::path::Path::new("draft.ord"));

    assert_eq!(
        destination.file_name().and_then(|name| name.to_str()),
        Some("draft 2.ord")
    );
}

#[test]
fn find_save_as_and_status_shortcuts_open_their_prompts() {
    use crate::prompt::PromptKind;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let mut app =
        with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
    app.state.add_draft(EditorBuffer::from_text("hello"), None);

    app.handle_key_event(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL));
    assert!(matches!(
        app.state.ui.prompt.as_ref().map(|prompt| &prompt.kind),
        Some(PromptKind::FindText)
    ));
    app.state.ui.prompt = None;

    app.handle_key_event(KeyEvent::new(
        KeyCode::Char('s'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    ));
    assert!(matches!(
        app.state.ui.prompt.as_ref().map(|prompt| &prompt.kind),
        Some(PromptKind::SavePath)
    ));
    app.state.ui.prompt = None;

    app.handle_key_event(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::ALT));
    assert!(matches!(
        app.state.ui.prompt.as_ref().map(|prompt| &prompt.kind),
        Some(PromptKind::DocumentStatus)
    ));
}

#[test]
fn ctrl_r_renames_in_the_browser_instead_of_trashing_the_page() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let dir = unique_temp_path("ordet-browser-rename", "");
    fs::create_dir_all(&dir).expect("browser dir");
    let file = dir.join("draft.txt");
    fs::write(&file, "x").expect("browser file");
    let mut app =
        with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
    app.state
        .add_draft(EditorBuffer::from_text("x"), Some(file.clone()));
    app.open_file_prompt();
    assert!(app.state.ui.file_browser.is_some());

    app.handle_key_event(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));

    assert!(app.state.ui.prompt.is_some(), "Ctrl-R opens Rename");
    assert!(app.state.ui.file_browser.is_some(), "browser stays open");
    assert_eq!(app.state.ui.pending_trash, None);
}

#[test]
fn browser_target_input_resolves_relative_to_the_source_parent() {
    let source = PathBuf::from("/tmp/ordet-browser/chapter1.ord");

    let target = parse_browser_target_input("chapter2.ord", &source).expect("target");

    assert_eq!(target, PathBuf::from("/tmp/ordet-browser/chapter2.ord"));
}

#[test]
fn copy_suggestion_adds_a_copy_suffix() {
    let suggestion = suggest_copy_destination(Path::new("/tmp/ordet-browser/chapter1.ord"));
    assert_eq!(
        suggestion.file_name().and_then(|name| name.to_str()),
        Some("chapter1 copy.ord")
    );
}

#[test]
fn pasted_paths_with_escaped_spaces_are_unescaped() {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("ordet dropped {stamp}.ord"));
    fs::write(&path, "note").expect("text fixture");
    let escaped = path.to_string_lossy().replace(' ', "\\ ");

    let parsed = parse_pasted_single_path(&escaped).expect("parsed path");

    assert_eq!(parsed, super::normalize_document_path(path));
}

#[test]
fn pasted_file_uri_with_localhost_is_parsed() {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("ordet-dropped-{stamp}.ord"));
    fs::write(&path, "note").expect("text fixture");
    let uri = format!("file://localhost{}", path.display());

    let parsed = parse_pasted_single_path(&uri).expect("parsed path");

    assert_eq!(parsed, super::normalize_document_path(path));
}

#[test]
fn pasted_wrapped_file_uri_inside_text_is_parsed() {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time")
        .as_nanos();
    let path = std::env::temp_dir().join(format!("ordet-dropped-{stamp}.ord"));
    fs::write(&path, "note").expect("text fixture");
    let uri = format!("Dropped: <file://{}>", path.display());

    let parsed = parse_pasted_single_path(&uri).expect("parsed path");

    assert_eq!(parsed, super::normalize_document_path(path));
}

fn unique_temp_path(prefix: &str, extension: &str) -> std::path::PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("{prefix}-test"));
    fs::create_dir_all(&root).expect("test temp root");
    if extension.is_empty() {
        root.join(format!("artifact-{stamp}"))
    } else {
        root.join(format!("artifact-{stamp}.{extension}"))
    }
}

#[test]
fn test_settings_override_outlives_the_construction_closure() {
    // The app keeps writing long after construction (settings saves, quit
    // backups, recovery snapshots). A regression here would send those
    // writes into the real user config directory (Application Support on
    // macOS, %APPDATA% on Windows), which is exactly how the Backups dir
    // filled up with test artifacts.
    with_test_settings(|| {});
    let path = std::env::var_os("ORDET_TUI_SETTINGS_PATH")
        .expect("the settings override must outlive the closure");
    let path = std::path::PathBuf::from(path);
    assert!(
        path.starts_with(std::env::temp_dir()),
        "post-closure writes must stay under the temp dir, got {}",
        path.display()
    );
}

fn with_test_settings<T>(run: impl FnOnce() -> T) -> T {
    // Settings lock first: the same order every test uses, so
    // opposite-order deadlocks are impossible.
    let guard = crate::settings::settings_env_lock()
        .lock()
        .expect("test settings lock");
    let settings_path = unique_temp_path("ordet-settings", "json");
    if settings_path.exists() {
        fs::remove_file(&settings_path).expect("clear stale test settings");
    }
    unsafe {
        std::env::set_var("ORDET_TUI_SETTINGS_PATH", &settings_path);
    }
    let result = run();
    // The settings override stays in place on purpose: the app keeps
    // writing after the closure returns (settings saves, quit backups,
    // recovery snapshots), and those writes must land in the temp dir
    // too, never in the real user config directory.
    drop(guard);
    result
}

#[test]
fn loading_an_ord_file_keeps_its_metadata() {
    let path = unique_temp_path("ordet-docmeta", "ord");
    write_to_path(
        &path,
        &OrdetDocument {
            title: "Chapter One".to_string(),
            status: "Done".to_string(),
            auto_title: false,
            label_color: "blue".to_string(),
            content_html: "<p>once upon a time</p>".to_string(),
            plain_text: "once upon a time".to_string(),
        },
    )
    .expect("write test ord");

    let loaded = load_buffer_from_path(&path).expect("load test ord");
    assert_eq!(loaded.buffer.plain_text(), "once upon a time");
    assert_eq!(loaded.buffer.cursor(), loaded.buffer.char_count());
    assert_eq!(loaded.metadata.status, "Done");
    assert!(!loaded.metadata.auto_title);
    assert_eq!(loaded.metadata.label_color, "blue");
}

#[test]
fn saving_an_ord_file_preserves_its_metadata() {
    let path = unique_temp_path("ordet-docmeta-save", "ord");
    write_to_path(
        &path,
        &OrdetDocument {
            title: "Chapter One".to_string(),
            status: "In Progress".to_string(),
            auto_title: false,
            label_color: "green".to_string(),
            content_html: "<p>once upon a time</p>".to_string(),
            plain_text: "once upon a time".to_string(),
        },
    )
    .expect("write test ord");

    let mut app = with_test_settings(|| {
        OrdetApp::from_args(vec![path.to_string_lossy().into_owned()]).expect("app from ord path")
    });
    assert_eq!(app.state.editor().doc_status, "In Progress");
    assert_eq!(app.state.editor().doc_label_color, "green");

    // An edit plus a background save must keep the loaded metadata while
    // storing the new text.
    app.state.editor_mut().buffer.move_cursor_to_end();
    app.state.editor_mut().buffer.insert_text(" and then");
    app.state.editor_mut().mark_dirty();
    app.spawn_save_to(path.clone());

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut saved = false;
    while !saved && std::time::Instant::now() < deadline {
        while let Some(task) = app.tasks.try_recv() {
            if matches!(
                task.result,
                Ok(crate::commands::AppMessage::SaveCompleted { .. })
            ) {
                saved = true;
                break;
            }
        }
        if !saved {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
    assert!(saved, "save task completed");

    let round_tripped = read_from_path(&path).expect("read saved ord");
    assert_eq!(round_tripped.status, "In Progress");
    assert!(!round_tripped.auto_title);
    assert_eq!(round_tripped.label_color, "green");
    assert_eq!(round_tripped.plain_text, "once upon a time and then");
}

#[test]
fn cli_args_split_flags_from_files() {
    let args = ["--config", "/tmp/x.json", "a.ord", "--", "--weird.ord"]
        .iter()
        .map(|arg| arg.to_string())
        .collect::<Vec<_>>();
    assert_eq!(
        parse_cli_args(&args),
        CliAction::Run {
            files: vec!["a.ord".to_string(), "--weird.ord".to_string()],
            config: Some("/tmp/x.json".to_string()),
            latency_log: false,
        }
    );

    let help = ["--help".to_string()];
    assert_eq!(parse_cli_args(&help), CliAction::Help);
    let version = ["-V".to_string()];
    assert_eq!(parse_cli_args(&version), CliAction::Version);

    let missing = ["--config".to_string()];
    assert!(matches!(parse_cli_args(&missing), CliAction::UsageError(_)));
    let unknown = ["--frobnicate".to_string()];
    assert!(matches!(parse_cli_args(&unknown), CliAction::UsageError(_)));
    let print = [
        "--config=/tmp/y.json".to_string(),
        "--print-config".to_string(),
    ];
    assert_eq!(
        parse_cli_args(&print),
        CliAction::PrintConfig {
            config: Some("/tmp/y.json".to_string())
        }
    );
}

#[test]
fn macos_option_glyphs_resolve_to_their_alt_chords() {
    use crate::commands::Command;
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    let app =
        with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
    let press = |code| KeyEvent {
        code,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::empty(),
    };
    // (glyph typed, chord meant, command fired)
    let cases = [
        ('∫', 'b', Command::ToggleBold),
        ('ç', 'c', Command::CopySelection),
        ('´', 'e', Command::ToggleItalic),
        ('µ', 'm', Command::ToggleMenuBar),
        ('ß', 's', Command::OpenStatusPrompt),
        ('†', 't', Command::InsertTimestamp),
        ('¨', 'u', Command::CycleCase),
        ('√', 'v', Command::PasteClipboard),
        ('∑', 'w', Command::CloseWindow),
        ('≈', 'x', Command::CutSelection),
    ];
    for (glyph, letter, command) in cases {
        let normalized = super::normalize_macos_option_key(press(KeyCode::Char(glyph)));
        assert_eq!(normalized.code, KeyCode::Char(letter), "glyph {glyph}");
        assert_eq!(normalized.modifiers, KeyModifiers::ALT);
        assert_eq!(app.keymap.resolve(normalized), Some(command));
    }
    // Untouched: ordinary text, other glyphs, already-modified keys.
    assert_eq!(
        super::normalize_macos_option_key(press(KeyCode::Char('é'))).code,
        KeyCode::Char('é')
    );
    assert_eq!(
        super::normalize_macos_option_key(press(KeyCode::Enter)).code,
        KeyCode::Enter
    );
    let alt_c = KeyEvent {
        code: KeyCode::Char('c'),
        modifiers: KeyModifiers::ALT,
        kind: KeyEventKind::Press,
        state: KeyEventState::empty(),
    };
    assert_eq!(super::normalize_macos_option_key(alt_c), alt_c);
}

#[test]
fn ctrl_a_selects_all_with_a_visible_selection() {
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    let mut app =
        with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
    app.state.add_draft(EditorBuffer::new(), None);
    app.state.editor_mut().buffer = EditorBuffer::from_text("hello world");
    let key_ctrl_a = KeyEvent {
        code: KeyCode::Char('a'),
        modifiers: KeyModifiers::CONTROL,
        kind: KeyEventKind::Press,
        state: KeyEventState::empty(),
    };
    app.handle_key_event(key_ctrl_a);
    // The whole buffer is selected; word movement now lives on the
    // Option-Left / Option-Right pair.
    assert_eq!(app.state.editor().buffer.selection_range(), Some((0, 11)));
}

#[test]
fn f9_toggles_the_stats_pane() {
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    let mut app =
        with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
    app.state.add_draft(EditorBuffer::new(), None);
    let key_f9 = KeyEvent {
        code: KeyCode::F(9),
        modifiers: KeyModifiers::empty(),
        kind: KeyEventKind::Press,
        state: KeyEventState::empty(),
    };
    app.handle_key_event(key_f9);
    assert!(app.state.panels.is(crate::state::PanelKind::Stats));
    app.handle_key_event(key_f9);
    assert!(!app.state.panels.is_open());
}

#[test]
fn ctrl_p_on_a_scratch_draft_opens_the_save_prompt() {
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    let mut app =
        with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
    app.state.add_draft(EditorBuffer::new(), None);
    let key_ctrl_p = KeyEvent {
        code: KeyCode::Char('p'),
        modifiers: KeyModifiers::CONTROL,
        kind: KeyEventKind::Press,
        state: KeyEventState::empty(),
    };
    app.handle_key_event(key_ctrl_p);
    let prompt = app.state.ui.prompt.as_ref().expect("save prompt");
    assert!(matches!(prompt.kind, crate::prompt::PromptKind::SavePath));
}

#[test]
fn ctrl_c_copies_and_ctrl_v_pastes_in_the_editor() {
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    let mut app =
        with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
    app.state.add_draft(EditorBuffer::new(), None);
    app.state.editor_mut().buffer = EditorBuffer::from_text("copy me");
    // Select-all via Ctrl-K A, copy via Ctrl-C.
    let key = |code, modifiers| KeyEvent {
        code,
        modifiers,
        kind: KeyEventKind::Press,
        state: KeyEventState::empty(),
    };
    app.handle_key_event(key(KeyCode::Char('k'), KeyModifiers::CONTROL));
    app.handle_key_event(key(KeyCode::Char('a'), KeyModifiers::NONE));
    assert!(app.state.editor().buffer.selection_range().is_some());
    app.handle_key_event(key(KeyCode::Char('c'), KeyModifiers::CONTROL));
    assert_eq!(app.state.clipboard.ring_top(), Some("copy me"));

    // Fresh buffer, no selection: Ctrl-V pastes the ring top.
    app.state.editor_mut().buffer = EditorBuffer::from_text("copy me");
    app.handle_key_event(key(KeyCode::Char('v'), KeyModifiers::CONTROL));
    assert_eq!(app.state.editor().buffer.plain_text(), "copy mecopy me");
}

#[test]
fn ctrl_a_ctrl_c_ctrl_v_duplicates_in_a_single_paste() {
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    let mut app =
        with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
    app.state.add_draft(EditorBuffer::new(), None);
    app.state.editor_mut().buffer = EditorBuffer::from_text("copy me");
    let key = |code, modifiers| KeyEvent {
        code,
        modifiers,
        kind: KeyEventKind::Press,
        state: KeyEventState::empty(),
    };
    // Select all, copy: the highlight must clear so the next paste
    // inserts instead of replacing the just-copied text.
    app.handle_key_event(key(KeyCode::Char('a'), KeyModifiers::CONTROL));
    assert!(app.state.editor().buffer.selection_range().is_some());
    app.handle_key_event(key(KeyCode::Char('c'), KeyModifiers::CONTROL));
    assert!(app.state.editor().buffer.selection_range().is_none());

    app.handle_key_event(key(KeyCode::Char('v'), KeyModifiers::CONTROL));
    assert_eq!(app.state.editor().buffer.plain_text(), "copy mecopy me");
}

#[test]
fn quit_asks_for_untitled_but_autosaves_existing_files() {
    let mut app =
        with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
    // Untitled dirty draft: quit must ask, not silently save.
    app.state.add_draft(EditorBuffer::new(), None);
    app.state.editor_mut().buffer.insert_text("unsaved work");
    app.state.editor_mut().mark_dirty();
    app.request_quit();
    assert!(!app.state.ui.should_quit);
    assert!(app.state.ui.quit_confirmation.is_some());

    // Existing file dirty draft: quit flushes and leaves quietly.
    let mut app =
        with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
    let path = std::env::temp_dir().join(format!(
        "ordet-quit-test-{}-existing.txt",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    app.state.add_draft(
        crate::editor_buffer::EditorBuffer::new(),
        Some(path.clone()),
    );
    app.state.editor_mut().buffer.insert_text("saved work");
    app.state.editor_mut().mark_dirty();
    app.request_quit();
    assert!(app.state.ui.should_quit);
    assert!(app.state.ui.quit_confirmation.is_none());
    assert_eq!(
        std::fs::read_to_string(&path).expect("saved file"),
        "saved work"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn quit_n_discards_and_quits_without_saving() {
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    let mut app =
        with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
    app.state.add_draft(EditorBuffer::new(), None);
    app.state.editor_mut().buffer.insert_text("unsaved work");
    app.state.editor_mut().mark_dirty();
    app.request_quit();
    assert!(app.state.ui.quit_confirmation.is_some());

    app.handle_key_event(KeyEvent {
        code: KeyCode::Char('n'),
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::empty(),
    });
    assert!(app.state.ui.should_quit);
    assert!(app.state.ui.quit_confirmation.is_none());
}

#[test]
fn quit_y_opens_save_prompt_for_untitled() {
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    let mut app =
        with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
    app.state.add_draft(EditorBuffer::new(), None);
    app.state.editor_mut().buffer.insert_text("unsaved work");
    app.state.editor_mut().mark_dirty();
    app.request_quit();
    assert!(app.state.ui.quit_confirmation.is_some());

    app.handle_key_event(KeyEvent {
        code: KeyCode::Char('y'),
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::empty(),
    });
    // Y saves: no quit yet, a save-path prompt is open instead, with the
    // quit resuming once the save lands.
    assert!(!app.state.ui.should_quit);
    assert!(app.state.ui.quit_confirmation.is_none());
    assert!(
        matches!(
            app.state.ui.prompt.as_ref().map(|prompt| &prompt.kind),
            Some(crate::prompt::PromptKind::SavePath)
        ),
        "Y must open a save prompt for untitled drafts"
    );
    assert!(app.state.ui.quit_after_save);
}

#[test]
fn quit_y_saves_backed_files_and_quits() {
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    let mut app =
        with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
    let path =
        std::env::temp_dir().join(format!("ordet-quit-test-{}-y-save.txt", std::process::id()));
    let _ = std::fs::remove_file(&path);
    // Seed the file first so the quit prompt comes from the failure path:
    // point the draft at an unwritable directory target instead.
    let dir = std::env::temp_dir().join(format!("ordet-quit-test-{}-y-dir", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    app.state
        .add_draft(crate::editor_buffer::EditorBuffer::new(), Some(dir.clone()));
    app.state.editor_mut().buffer.insert_text("x");
    app.state.editor_mut().mark_dirty();
    app.request_quit();
    assert!(app.state.ui.quit_confirmation.is_some());

    // N discards even failed writes and quits.
    app.handle_key_event(KeyEvent {
        code: KeyCode::Char('n'),
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::empty(),
    });
    assert!(app.state.ui.should_quit);
    let _ = std::fs::remove_dir(&dir);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn zoom_hides_the_chrome_and_panels_leave_zoom_first() {
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    let mut app =
        with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
    app.state.add_draft(EditorBuffer::new(), None);
    assert!(!app.state.ui.zoomed);
    let esc = || KeyEvent {
        code: KeyCode::Esc,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::empty(),
    };

    app.toggle_zoom();
    assert!(app.state.ui.zoomed);

    // Asking for a panel while zoomed restores the full layout first.
    app.toggle_panel(crate::state::PanelKind::Notes);
    assert!(!app.state.ui.zoomed);
    assert!(app.state.panels.is(crate::state::PanelKind::Notes));

    app.toggle_zoom();
    assert!(app.state.ui.zoomed);

    // Esc leaves zoom first, then closes the panel: the overlay cascade.
    app.handle_key_event(esc());
    assert!(!app.state.ui.zoomed);
    assert!(app.state.panels.is(crate::state::PanelKind::Notes));
    app.handle_key_event(esc());
    assert!(!app.state.panels.is_open());
}

#[test]
fn option_m_hides_the_menu_bar_and_persists_it() {
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    // This test reads the settings file back after a save, so parallel
    // tests must not repoint `ORDET_TUI_SETTINGS_PATH` between the write
    // and the read. `with_test_settings` releases the lock when its
    // closure returns (construction only); here the whole test holds it.
    // `from_args` runs directly because `with_test_settings` would try to
    // take the already-held lock and deadlock.
    let guard = crate::settings::settings_env_lock()
        .lock()
        .expect("test settings lock");
    let settings_path = unique_temp_path("ordet-settings", "json");
    if settings_path.exists() {
        fs::remove_file(&settings_path).expect("clear stale test settings");
    }
    unsafe {
        std::env::set_var("ORDET_TUI_SETTINGS_PATH", &settings_path);
    }
    let mut app = OrdetApp::from_args(Vec::new()).expect("app with scratch draft");
    app.state.add_draft(EditorBuffer::new(), None);
    assert!(app.state.ui.show_menu_bar);

    let option_m = || KeyEvent {
        code: KeyCode::Char('m'),
        modifiers: KeyModifiers::ALT,
        kind: KeyEventKind::Press,
        state: KeyEventState::empty(),
    };

    app.handle_key_event(option_m());
    assert!(!app.state.ui.show_menu_bar, "⌥M hides the bar");
    assert!(
        !crate::settings::load_settings()
            .expect("settings load")
            .show_menu_bar,
        "the hidden bar is remembered"
    );

    app.handle_key_event(option_m());
    assert!(app.state.ui.show_menu_bar, "⌥M shows it again");
    assert!(
        crate::settings::load_settings()
            .expect("settings load")
            .show_menu_bar,
        "showing it is remembered too"
    );
    // Release the lock after the last read; the override itself stays
    // set by design (see with_test_settings' comment).
    drop(guard);
}

#[test]
fn settings_toggles_flip_and_persist() {
    let mut app =
        with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
    assert!(app.state.ui.show_live_counts);
    assert!(app.state.ui.backup_enabled);

    app.toggle_live_counts();
    assert!(!app.state.ui.show_live_counts);
    app.toggle_backup();
    assert!(!app.state.ui.backup_enabled);
    app.toggle_backup_on_close();
    assert!(!app.state.ui.backup_on_close);

    let keep = app.state.ui.backup_keep;
    app.cycle_backup_keep();
    assert_ne!(app.state.ui.backup_keep, keep);
}

#[test]
fn page_rows_stay_clickable_with_live_counts_off() {
    use ordet_tui_framework::{Point, Rect};

    let mut app =
        with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
    app.state.add_draft(EditorBuffer::new(), None);
    app.state.panels.open(PanelKind::Pages);
    app.state.ui.show_live_counts = false;
    app.state.update_layout(Rect::new(0, 0, 120, 60));
    let area = crate::state::panel_rect(&app.state);
    let mut seen = std::collections::BTreeSet::new();
    for y in area.y..area.bottom() {
        if let Some(index) =
            crate::components::panel_row_at_point(&app.state, Point::new(area.x + 2, y))
        {
            seen.insert(index);
        }
    }
    assert_eq!(
        seen.len(),
        app.state.page_rows().len(),
        "every page row stays clickable with counts off"
    );
}

#[test]
fn backup_location_prompt_resets_and_rejects() {
    let mut app =
        with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
    // Empty resets to the default folder.
    app.state.ui.backup_dir = std::env::temp_dir()
        .join("ordet-custom")
        .display()
        .to_string();
    app.open_backup_location_prompt();
    let prompt = app.state.ui.prompt.take().expect("prompt");
    assert!(matches!(
        prompt.kind,
        crate::prompt::PromptKind::BackupDirectory
    ));
    app.submit_prompt(crate::prompt::PromptState {
        value: "   ".to_string(),
        ..prompt
    });
    assert!(app.state.ui.backup_dir.is_empty());
    assert!(app.state.ui.prompt.is_none());

    // An uncreatable path reopens the prompt instead of saving. A file
    // in the way fails the same way on every platform, unlike a
    // Unix-only pseudo-filesystem path.
    let blocker = unique_temp_path("ordet-backup-blocker", "txt");
    fs::write(&blocker, "not a directory").expect("blocker file");
    let impossible = blocker.join("child").display().to_string();
    app.open_backup_location_prompt();
    let prompt = app.state.ui.prompt.take().expect("prompt");
    app.submit_prompt(crate::prompt::PromptState {
        value: impossible,
        ..prompt
    });
    assert!(app.state.ui.prompt.is_some(), "bad path reopens");
    assert!(app.state.ui.backup_dir.is_empty(), "bad path not saved");
    let _ = fs::remove_file(blocker);
}

#[test]
fn tab_on_a_clean_paragraph_still_indents() {
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    let mut app =
        with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
    app.state.add_draft(EditorBuffer::new(), None);
    app.state
        .editor_mut()
        .buffer
        .insert_text("The cat sat on the mat.");
    app.handle_key_event(KeyEvent {
        code: KeyCode::Tab,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::empty(),
    });
    // A Tab with nothing to fix puts an indent at the cursor.
    assert_eq!(
        app.state.editor().buffer.plain_text(),
        "The cat sat on the mat.    "
    );
}

#[test]
fn tab_at_a_line_start_indents() {
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    let mut app =
        with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
    app.state.add_draft(EditorBuffer::new(), None);
    app.state
        .editor_mut()
        .buffer
        .insert_text("i will come to kevins house");
    app.state.editor_mut().buffer.move_to_top();
    app.handle_key_event(KeyEvent {
        code: KeyCode::Tab,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::empty(),
    });
    // A line start reads as indent intent.
    assert_eq!(
        app.state.editor().buffer.plain_text(),
        "    i will come to kevins house"
    );
}

#[test]
fn quit_backs_up_saved_files_and_prunes() {
    let mut app =
        with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
    let root = std::env::temp_dir().join(format!("ordet-backup-quit-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("temp dir");
    let path = root.join("draft.txt");
    std::fs::write(&path, "v1").expect("seed");
    let backups = root.join("Backups");
    app.state.ui.backup_dir = backups.display().to_string();
    app.state.ui.backup_keep = 2;
    app.state.add_draft(
        crate::editor_buffer::EditorBuffer::new(),
        Some(path.clone()),
    );
    app.state.editor_mut().buffer.insert_text("v2");
    app.state.editor_mut().mark_dirty();

    // Two quits with edits between them must leave two backups, then
    // pruning keeps only the newest two of three.
    app.request_quit();
    assert!(app.state.ui.should_quit);
    app.state.ui.should_quit = false;
    app.state.editor_mut().buffer.insert_text(" more");
    app.state.editor_mut().mark_dirty();
    app.request_quit();
    app.state.ui.should_quit = false;
    app.state.editor_mut().buffer.insert_text(" again");
    app.state.editor_mut().mark_dirty();
    app.request_quit();
    let count = std::fs::read_dir(&backups)
        .expect("read")
        .filter_map(|entry| entry.ok())
        .count();
    assert_eq!(count, 2, "pruning must keep newest two");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn autosave_skips_untitled_drafts() {
    let mut app =
        with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
    app.state.add_draft(EditorBuffer::new(), None);
    app.state.editor_mut().buffer.insert_text("untitled work");
    app.state.editor_mut().mark_dirty();
    app.next_autosave_at = std::time::Instant::now() - std::time::Duration::from_secs(10);
    app.maybe_autosave();
    assert!(
        !app.state.editor().pending_save,
        "untitled drafts must never autosave"
    );
}

#[test]
fn ctrl_v_pastes_on_the_first_press_in_every_encoding() {
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    let key = |code, modifiers| KeyEvent {
        code,
        modifiers,
        kind: KeyEventKind::Press,
        state: KeyEventState::empty(),
    };
    // Every Ctrl-V and Alt-V encoding is paste, whatever the terminal
    // reports for the letter case or the Shift flag.
    for (code, modifiers) in [
        (KeyCode::Char('v'), KeyModifiers::CONTROL),
        (KeyCode::Char('V'), KeyModifiers::CONTROL),
        (
            KeyCode::Char('v'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ),
        (
            KeyCode::Char('V'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ),
        (KeyCode::Char('v'), KeyModifiers::ALT),
        (KeyCode::Char('V'), KeyModifiers::ALT),
    ] {
        assert!(
            super::is_paste_key(key(code, modifiers)),
            "{code:?} {modifiers:?}"
        );
    }
    assert!(!super::is_paste_key(key(
        KeyCode::Char('v'),
        KeyModifiers::NONE
    )));

    // One press pastes even with a Ctrl-K prefix armed: the prefix is
    // cleared and the paste lands in the same press, not the next one.
    // Note: `paste()` prefers the real system clipboard, so tests assert
    // a paste happened (non-empty + prefix cleared), not exact text.
    let mut app =
        with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
    app.state.add_draft(EditorBuffer::new(), None);
    app.state.clipboard.copy("pasted");
    app.state.editor_mut().buffer = EditorBuffer::from_text("");
    app.handle_key_event(key(KeyCode::Char('k'), KeyModifiers::CONTROL));
    assert!(app.state.ui.prefix.is_some());
    app.handle_key_event(key(KeyCode::Char('v'), KeyModifiers::CONTROL));
    assert!(
        !app.state.editor().buffer.plain_text().is_empty(),
        "first Ctrl-V with prefix armed must paste in the same press"
    );
    assert!(app.state.ui.prefix.is_none());

    // Alt-V variants paste on the first press too.
    for code in [KeyCode::Char('v'), KeyCode::Char('V')] {
        let mut app =
            with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
        app.state.add_draft(EditorBuffer::new(), None);
        app.state.clipboard.copy("alt-paste");
        app.state.editor_mut().buffer = EditorBuffer::from_text("");
        app.handle_key_event(key(code, KeyModifiers::ALT));
        assert!(
            !app.state.editor().buffer.plain_text().is_empty(),
            "Alt-V {code:?} must paste first press"
        );
    }
}

#[test]
fn ctrl_v_pastes_into_prompt_and_browser_on_first_press() {
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    let key = |code, modifiers| KeyEvent {
        code,
        modifiers,
        kind: KeyEventKind::Press,
        state: KeyEventState::empty(),
    };
    // Prompt: first Ctrl-V lands in the prompt value (system clipboard
    // wins over the ring, so assert non-empty rather than exact text).
    let mut app =
        with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
    app.state.add_draft(EditorBuffer::new(), None);
    app.state.clipboard.copy("prompt-text");
    app.state.ui.prompt = Some(crate::prompt::PromptState::find(Some("")));
    app.handle_key_event(key(KeyCode::Char('v'), KeyModifiers::CONTROL));
    let prompt = app.state.ui.prompt.as_ref().expect("prompt stays open");
    assert!(
        !prompt.value.is_empty(),
        "first Ctrl-V must fill prompt, got {:?}",
        prompt.value
    );
}

#[test]
fn macos_option_c_copies_the_editor_selection() {
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    let mut app =
        with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
    app.state.add_draft(EditorBuffer::new(), None);
    app.state.editor_mut().buffer = EditorBuffer::from_text("copy me");
    // Select-all in the editor (Ctrl-K A), then Option+C (arrives as ç).
    let key_ctrl_k = KeyEvent {
        code: KeyCode::Char('k'),
        modifiers: KeyModifiers::CONTROL,
        kind: KeyEventKind::Press,
        state: KeyEventState::empty(),
    };
    let key_a = KeyEvent {
        code: KeyCode::Char('a'),
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::empty(),
    };
    app.handle_key_event(key_ctrl_k);
    app.handle_key_event(key_a);
    assert!(app.state.editor().buffer.selection_range().is_some());
    let option_c = KeyEvent {
        code: KeyCode::Char('ç'),
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::empty(),
    };
    app.handle_key_event(option_c);
    assert_eq!(app.state.clipboard.ring_top(), Some("copy me"));
}

#[test]
fn f6_hops_pages_and_shift_f6_hops_back() {
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
    use ordet_tui_framework::Rect;

    let mut app =
        with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
    app.state.add_draft(EditorBuffer::from_text("one"), None);
    app.state.add_draft(EditorBuffer::from_text("two"), None);
    app.state.add_draft(EditorBuffer::from_text("three"), None);
    app.state.update_layout(Rect::new(0, 0, 120, 30));
    assert_eq!(app.state.editor().title(), "three");

    let press = |code, modifiers| KeyEvent {
        code,
        modifiers,
        kind: KeyEventKind::Press,
        state: KeyEventState::empty(),
    };
    // F6 wraps from the last page to the first.
    app.handle_key_event(press(KeyCode::F(6), KeyModifiers::NONE));
    assert_eq!(app.state.editor().title(), "one");
    // Shift-F6 steps back.
    app.handle_key_event(press(KeyCode::F(6), KeyModifiers::SHIFT));
    assert_eq!(app.state.editor().title(), "three");
    // A single visible page says so instead of pretending to move.
    let mut solo =
        with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
    solo.state.add_draft(EditorBuffer::from_text("only"), None);
    assert_eq!(solo.state.visible_draft_count(), 1);
    solo.handle_key_event(press(KeyCode::F(6), KeyModifiers::NONE));
    assert!(
        solo.state.ui.status.contains("Only one page"),
        "{}",
        solo.state.ui.status
    );
}

#[test]
fn pages_panel_enter_switches_and_closes() {
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    let mut app =
        with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
    app.state.add_draft(EditorBuffer::from_text("one"), None);
    app.state.add_draft(EditorBuffer::from_text("two"), None);
    app.toggle_panel(PanelKind::Pages);
    assert_eq!(app.state.panels.pages_selected, 1);

    // Enter on the first row switches the editor there and the panel
    // steps out of the way so typing resumes immediately.
    app.state.panels.pages_selected = 0;
    app.handle_key_event(KeyEvent {
        code: KeyCode::Enter,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::empty(),
    });
    assert_eq!(app.state.editor().title(), "one");
    assert!(!app.state.panels.is_open());
}

#[test]
fn alt_n_splits_the_desktop_and_alt_w_closes_it() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let mut app =
        with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
    app.state.add_draft(EditorBuffer::from_text("one"), None);
    app.state.add_draft(EditorBuffer::from_text("two"), None);
    app.state.update_layout(Rect::new(0, 0, 120, 30));

    // The next listed page opens beside the current one and takes the
    // keyboard.
    app.handle_key_event(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::ALT));
    assert_eq!(app.state.window_count(), 2);
    assert_eq!(app.state.ui.layout.window_rects.len(), 2);
    assert_eq!(app.state.editor().title(), "one");
    assert!(
        app.state.ui.status.is_empty(),
        "splitting is routine and stays quiet: {}",
        app.state.ui.status
    );

    // F6 walks the desktop once it is split, and the split survives.
    app.handle_key_event(KeyEvent::new(KeyCode::F(6), KeyModifiers::empty()));
    assert_eq!(app.state.editor().title(), "two");
    assert_eq!(app.state.window_count(), 2);

    app.handle_key_event(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::ALT));
    assert_eq!(app.state.window_count(), 1);
    assert_eq!(app.state.ui.layout.window_rects.len(), 1);
}

#[test]
fn alt_n_opens_four_windows_then_cycles() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let mut app =
        with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
    for title in ["one", "two", "three", "four", "five"] {
        app.state.add_draft(EditorBuffer::from_text(title), None);
    }
    app.state.update_layout(Rect::new(0, 0, 120, 30));

    // Each press opens the next unopened page, up to four windows.
    for (title, windows, position) in [("one", 2, 2), ("two", 3, 3), ("three", 4, 4)] {
        app.handle_key_event(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::ALT));
        assert_eq!(app.state.window_count(), windows);
        assert_eq!(app.state.ui.layout.window_rects.len(), windows);
        assert_eq!(app.state.active_window_position() + 1, position);
        assert_eq!(app.state.editor().title(), title);
    }

    // A fifth page cannot open on a full desktop; Alt-N cycles instead.
    app.handle_key_event(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::ALT));
    assert_eq!(app.state.window_count(), 4);
    assert_eq!(app.state.editor().title(), "five");
    assert!(
        app.state.ui.status.is_empty(),
        "cycling is routine and stays quiet: {}",
        app.state.ui.status
    );
}

#[test]
fn window_menu_moves_the_focused_page_to_another_slot() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let mut app =
        with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
    for title in ["one", "two", "three", "four"] {
        app.state.add_draft(EditorBuffer::from_text(title), None);
    }
    app.state.update_layout(Rect::new(0, 0, 120, 30));
    for _ in 0..3 {
        app.handle_key_event(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::ALT));
    }
    assert_eq!(app.state.active_window_position(), 3);
    assert_eq!(app.state.editor().title(), "three");

    // Window ▸ Move to slot 1 swaps the focused page into the first tile.
    app.run_menu_item(6, 2);
    assert_eq!(app.state.active_window_position(), 0);
    assert_eq!(app.state.editor().title(), "three");
    let labels: Vec<String> = app
        .state
        .window_drafts()
        .iter()
        .map(|index| app.state.drafts[*index].page_label())
        .collect();
    assert_eq!(labels, vec!["three", "one", "two", "four"]);
    assert!(
        app.state.ui.status.is_empty(),
        "moving a tile is routine and stays quiet: {}",
        app.state.ui.status
    );
}

#[test]
fn ctrl_o_aligns_the_paragraph_without_a_prompt() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let mut app =
        with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
    app.state.add_draft(EditorBuffer::from_text("hello"), None);

    // The on-screen formatting prefix: ⌃O C centers the line.
    app.handle_key_event(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL));
    assert!(app.state.ui.prefix.is_some(), "⌃O starts a prefix");
    app.handle_key_event(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::NONE));
    assert!(app.state.ui.prefix.is_none(), "the stroke ends the prefix");
    assert_eq!(
        app.state.editor().buffer.current_alignment(),
        ParagraphAlignment::Center
    );

    // Right-align, then a stray letter: cancels and leaves the line alone.
    app.handle_key_event(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL));
    app.handle_key_event(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE));
    assert_eq!(
        app.state.editor().buffer.current_alignment(),
        ParagraphAlignment::Right
    );
    app.handle_key_event(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL));
    app.handle_key_event(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE));
    assert!(app.state.ui.prefix.is_none());
    assert_eq!(
        app.state.editor().buffer.current_alignment(),
        ParagraphAlignment::Right
    );
}

#[test]
fn f1_then_a_key_jumps_to_that_commands_help() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let mut app =
        with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
    app.state.add_draft(EditorBuffer::from_text("hello"), None);
    app.state.update_layout(Rect::new(0, 0, 120, 30));

    app.handle_key_event(KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE));
    assert_eq!(app.state.panels.panel, Some(PanelKind::Help));
    assert_eq!(app.state.panels.help_tab, HelpTab::Movement);
    assert_eq!(app.state.panels.scroll, 0);

    // The key is looked up, not run: Save must not write the draft out.
    let before = app.state.drafts.len();
    app.handle_key_event(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
    assert_eq!(
        app.state.panels.help_tab,
        HelpTab::Workspace,
        "Save lives on the Workspace page"
    );
    assert_eq!(app.state.drafts.len(), before);
    assert_eq!(app.state.panels.panel, Some(PanelKind::Help));

    // Tab, Shift+Tab and the horizontal arrows switch sections instead
    // of being looked up themselves.
    app.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    assert_eq!(app.state.panels.help_tab, HelpTab::Tools);
    app.handle_key_event(KeyEvent::new(KeyCode::Tab, KeyModifiers::SHIFT));
    assert_eq!(app.state.panels.help_tab, HelpTab::Workspace);
    app.handle_key_event(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    assert_eq!(app.state.panels.help_tab, HelpTab::Tools);
    app.handle_key_event(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
    assert_eq!(app.state.panels.help_tab, HelpTab::Workspace);

    // Up/Down are inert here, and an unknown key leaves the page alone.
    app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
    assert_eq!(app.state.panels.help_tab, HelpTab::Workspace);
    app.handle_key_event(KeyEvent::new(KeyCode::Char('\u{0}'), KeyModifiers::NONE));
    assert_eq!(app.state.panels.help_tab, HelpTab::Workspace);
}

#[test]
fn clicking_a_window_focuses_the_page_under_the_pointer() {
    use crossterm::event::{
        KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    };

    let mut app =
        with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
    app.state.add_draft(EditorBuffer::from_text("one"), None);
    app.state.add_draft(EditorBuffer::from_text("two"), None);
    app.state.update_layout(Rect::new(0, 0, 120, 30));
    app.handle_key_event(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::ALT));
    // The new window is the right one; page two sits on the left.
    assert_eq!(app.state.editor().title(), "one");
    assert_eq!(app.state.active_window_position(), 1);

    app.handle_mouse_event(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: 10,
        row: 5,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(app.state.active_window_position(), 0);
    assert_eq!(app.state.editor().title(), "two");
}

#[test]
fn alt_arrows_reorder_pages() {
    use crate::commands::Command;
    use ordet_tui_framework::UiEvent;

    let mut app =
        with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
    app.state.add_draft(EditorBuffer::from_text("one"), None);
    app.state.add_draft(EditorBuffer::from_text("two"), None);
    assert_eq!(app.state.editor().title(), "two");

    // Without the panel, Alt-Up moves the page being edited: "two"
    // trades places with "one" and stays current.
    app.dispatch(UiEvent::Command(Command::MovePageUp));
    let labels: Vec<String> = app
        .state
        .page_rows()
        .iter()
        .map(|row| row.label.clone())
        .collect();
    assert_eq!(labels, vec!["two".to_string(), "one".to_string()]);
    assert_eq!(app.state.editor().title(), "two");

    // At the top already: no move, and the status says why.
    app.dispatch(UiEvent::Command(Command::MovePageUp));
    assert_eq!(app.state.editor().title(), "two");
    assert!(
        app.state.ui.status.contains("first page"),
        "{}",
        app.state.ui.status
    );

    // With the panel open the chord acts on the highlighted row.
    app.state.panels.open(PanelKind::Pages);
    app.state.panels.pages_selected = 0;
    app.dispatch(UiEvent::Command(Command::MovePageDown));
    let labels: Vec<String> = app
        .state
        .page_rows()
        .iter()
        .map(|row| row.label.clone())
        .collect();
    assert_eq!(labels, vec!["one".to_string(), "two".to_string()]);

    // The bottom row cannot move down.
    app.state.panels.pages_selected = 1;
    app.dispatch(UiEvent::Command(Command::MovePageDown));
    assert!(
        app.state.ui.status.contains("last page"),
        "{}",
        app.state.ui.status
    );
}

#[test]
fn reordering_pages_keeps_in_flight_saves_pointing_at_the_right_draft() {
    use crate::commands::AppMessage;

    let dir = unique_temp_path("ordet-reorder-save", "");
    fs::create_dir_all(&dir).expect("temp dir");
    // Seed the files first: the app canonicalizes existing paths (macOS
    // temp dirs symlink /var to /private/var), so compare against that
    // canonical form.
    fs::write(dir.join("one.txt"), "one").expect("seed one");
    fs::write(dir.join("two.txt"), "two").expect("seed two");
    let one_path = crate::state::normalize_document_path(dir.join("one.txt"));
    let two_path = crate::state::normalize_document_path(dir.join("two.txt"));

    let mut app =
        with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
    let one = app
        .state
        .add_draft(EditorBuffer::from_text("one"), Some(one_path.clone()));
    let two = app
        .state
        .add_draft(EditorBuffer::from_text("two"), Some(two_path.clone()));
    let one_id = app.state.drafts[one].id;
    let two_id = app.state.drafts[two].id;

    // Kick off a save of the trailing page...
    app.state.editor_mut().buffer.insert_text(" edited");
    app.state.editor_mut().mark_dirty();
    app.spawn_save_to(two_path.clone());
    assert!(app.state.drafts[two].editor.pending_save);

    // ...reorder while it is in flight, then save the page that inherited
    // its old row. Index-keyed plumbing used to cancel the first save here
    // and stamp the second page's completion onto the wrong draft.
    assert!(app.state.move_draft(two, -1));
    assert_eq!(app.state.draft_index_by_id(two_id), Some(two - 1));
    let one_now = app.state.draft_index_by_id(one_id).expect("one open");
    app.state.switch_to_draft(one_now);
    app.spawn_save_to(one_path.clone());

    // Both completions must land...
    let mut completed_saves = 0usize;
    let mut results = Vec::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while completed_saves < 2 && std::time::Instant::now() < deadline {
        while let Some(task) = app.tasks.try_recv() {
            if matches!(task.result, Ok(AppMessage::SaveCompleted { .. })) {
                completed_saves += 1;
            }
            results.push(task);
        }
        if completed_saves < 2 {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
    assert_eq!(completed_saves, 2, "both in-flight saves must complete");
    for task in results {
        app.handle_task_result(task);
    }

    // ...and each on the page it was started for.
    let two_index = app.state.draft_index_by_id(two_id).expect("two open");
    let one_index = app.state.draft_index_by_id(one_id).expect("one open");
    assert_eq!(
        app.state.drafts[two_index].editor.file_path.as_deref(),
        Some(two_path.as_path()),
        "the save must land on the page it was started for"
    );
    assert_eq!(
        app.state.drafts[one_index].editor.file_path.as_deref(),
        Some(one_path.as_path()),
        "no page may inherit another page's path"
    );
    assert!(!app.state.drafts[two_index].editor.pending_save);
    assert!(!app.state.drafts[one_index].editor.pending_save);
}

#[test]
fn trash_arms_before_it_trashes() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let mut app =
        with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
    app.state.add_draft(EditorBuffer::new(), None);
    // Missing on disk on purpose: the confirm path then only touches
    // test state, never the filesystem.
    let missing = std::env::temp_dir().join("ordet-trash-arm-test-no-such-file.ord");
    app.state.editor_mut().file_path = Some(missing.clone());

    // The first trash command only arms: draft kept, nothing forgotten.
    app.dispatch(UiEvent::Command(Command::TrashFile));
    assert_eq!(app.state.ui.pending_trash, Some(missing.clone()));
    assert_eq!(app.state.editor().file_path, Some(missing.clone()));
    assert!(
        app.state
            .ui
            .status
            .contains("Are you sure you want to delete this page? Y/N"),
        "unexpected prompt: {}",
        app.state.ui.status
    );

    // Any other key disarms: the next trash command re-arms instead of firing.
    app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::empty()));
    assert_eq!(app.state.ui.pending_trash, None);
    app.dispatch(UiEvent::Command(Command::TrashFile));
    assert_eq!(app.state.ui.pending_trash, Some(missing.clone()));
    assert_eq!(app.state.editor().file_path, Some(missing.clone()));

    // N disarms without trashing and types nothing.
    app.handle_key_event(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::empty()));
    assert_eq!(app.state.ui.pending_trash, None);
    assert_eq!(app.state.editor().file_path, Some(missing.clone()));

    // Y fires: missing file, draft closed.
    app.dispatch(UiEvent::Command(Command::TrashFile));
    assert_eq!(app.state.ui.pending_trash, Some(missing.clone()));
    app.handle_key_event(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::empty()));
    assert_eq!(app.state.ui.pending_trash, None);
    assert_eq!(app.state.editor().file_path, None);

    // Second consecutive Ctrl+R still fires: missing file, draft closed.
    app.state.editor_mut().file_path = Some(missing.clone());
    app.dispatch(UiEvent::Command(Command::TrashFile));
    app.dispatch(UiEvent::Command(Command::TrashFile));
    assert_eq!(app.state.ui.pending_trash, None);
    assert_eq!(app.state.editor().file_path, None);
}

#[test]
fn delete_confirms_before_closing_even_a_clean_unsaved_page() {
    use crate::commands::Action;

    let mut app =
        with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
    app.state.add_draft(EditorBuffer::new(), None);

    // Delete asks...
    app.apply_action(Action::TrashRequested { confirm: false });
    assert_eq!(app.state.ui.pending_close_draft, Some(1));
    assert_eq!(app.state.drafts.len(), 2);

    // ...Delete again still only asks, never fires.
    app.apply_action(Action::TrashRequested { confirm: false });
    assert_eq!(app.state.ui.pending_close_draft, Some(1));
    assert_eq!(app.state.drafts.len(), 2);

    // ...only Ctrl+R confirms.
    app.apply_action(Action::TrashRequested { confirm: true });
    assert_eq!(app.state.ui.pending_close_draft, None);
    assert_eq!(app.state.drafts.len(), 1);
    assert_eq!(app.state.active_draft, 0);
    assert!(app.state.editor().buffer.plain_text().is_empty());
}

#[test]
fn delete_lands_on_the_surviving_page() {
    use crate::commands::Action;

    let mut app =
        with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
    app.state.add_draft(EditorBuffer::new(), None);
    app.state.add_draft(EditorBuffer::new(), None);
    assert_eq!(app.state.active_draft, 2);

    // On the last page: Delete asks, Y closes it, and the editor lands
    // on the surviving page instead of the removed slot.
    app.apply_action(Action::TrashRequested { confirm: false });
    assert_eq!(app.state.ui.pending_close_draft, Some(2));
    app.apply_action(Action::TrashRequested { confirm: true });
    assert_eq!(app.state.drafts.len(), 2);
    assert_eq!(app.state.active_draft, 1);
}

#[test]
fn delete_arms_before_discarding_a_dirty_unsaved_page() {
    use crate::commands::Action;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let mut app =
        with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
    app.state
        .add_draft(EditorBuffer::from_text("words here"), None);
    app.state.editor_mut().buffer.insert_text("more");
    app.state.editor_mut().mark_dirty();

    // First Delete only arms: the page survives.
    app.apply_action(Action::TrashRequested { confirm: false });
    assert_eq!(app.state.ui.pending_close_draft, Some(1));
    assert_eq!(app.state.drafts.len(), 2);

    // Any other key cancels the discard and wipes its message.
    app.handle_key_event(KeyEvent::new(KeyCode::Up, KeyModifiers::empty()));
    assert_eq!(app.state.ui.pending_close_draft, None);
    assert_eq!(app.state.drafts.len(), 2);
    assert!(
        !app.state.ui.status.contains("delete this page?"),
        "stale confirm message survived moving: {}",
        app.state.ui.status
    );

    // Delete asks, Y discards it.
    app.apply_action(Action::TrashRequested { confirm: false });
    assert!(
        app.state
            .ui
            .status
            .contains("Are you sure you want to delete this page? Y/N"),
        "unexpected prompt: {}",
        app.state.ui.status
    );
    app.handle_key_event(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::empty()));
    assert_eq!(app.state.ui.pending_close_draft, None);
    assert_eq!(app.state.drafts.len(), 1);

    // Delete asks, N keeps the page and types nothing.
    app.apply_action(Action::TrashRequested { confirm: false });
    assert_eq!(app.state.ui.pending_close_draft, Some(0));
    app.handle_key_event(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::empty()));
    assert_eq!(app.state.ui.pending_close_draft, None);
    assert_eq!(app.state.drafts.len(), 1);
}

#[test]
fn esc_closes_the_open_panel_but_yields_to_prefix() {
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

    let mut app =
        with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
    // Leave the welcome screen: panel Esc yields to it there.
    app.state.add_draft(EditorBuffer::new(), None);
    let esc = KeyEvent {
        code: KeyCode::Esc,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: crossterm::event::KeyEventState::NONE,
    };
    app.state.panels.open(PanelKind::Notes);
    app.handle_key_event(esc);
    assert!(!app.state.panels.is_open());

    app.handle_key_event(esc);
    assert!(!app.state.panels.is_open());

    // A pending Ctrl-K prefix owns Esc: it cancels the prefix instead.
    // The waiting hint is persistent, so it does not vanish after three
    // seconds while the chord is still armed.
    app.state.panels.open(PanelKind::Notes);
    app.handle_key_event(KeyEvent {
        code: KeyCode::Char('k'),
        modifiers: KeyModifiers::CONTROL,
        kind: KeyEventKind::Press,
        state: crossterm::event::KeyEventState::NONE,
    });
    assert_eq!(app.state.ui.prefix, Some(crate::state::PrefixMode::CtrlK));
    assert!(
        app.state.ui.status.starts_with("⌃K pressed"),
        "unexpected hint: {}",
        app.state.ui.status
    );
    assert!(app.state.ui.status_expires_at.is_none());
    app.handle_key_event(esc);
    assert!(app.state.panels.is(PanelKind::Notes));
    assert_eq!(app.state.ui.prefix, None);
    assert!(
        app.state.ui.status.starts_with("Notes."),
        "panel hint should return: {}",
        app.state.ui.status
    );
}

#[test]
fn mouse_click_drag_and_wheel_drive_the_editor() {
    use crate::components::editor_text_area;
    use ordet_tui_framework::{Point, Rect};

    let path = unique_temp_path("ordet-mouse", "txt");
    fs::write(&path, "alpha beta\ngamma delta").expect("write test file");
    let mut app = with_test_settings(|| {
        OrdetApp::from_args(vec![path.to_string_lossy().into_owned()]).expect("app from text path")
    });
    app.state.update_layout(Rect::new(0, 0, 80, 24));
    let text_area = editor_text_area(app.state.ui.layout.editor);

    // Click on the second visual line, a few columns in.
    let click = Point::new(text_area.x + 3, text_area.y + 1);
    app.mouse_press(click);
    let (wrap_width, _) = crate::components::wrap_and_gutter(
        text_area.width as usize,
        &app.state.editor().buffer,
        app.state.ui.show_line_numbers,
    );
    let expected = app
        .state
        .editor()
        .buffer
        .visual_cursor_for_column(wrap_width, 1, 3);
    assert_eq!(app.state.editor().buffer.cursor(), expected);

    // Drag extends a selection from the press point.
    app.mouse_drag(Point::new(text_area.x + 6, text_area.y + 1));
    let selection = app.state.editor().buffer.selection_range();
    assert!(selection.is_some());
    assert_ne!(selection.unwrap().0, selection.unwrap().1);
    app.mouse_drag(Point::new(text_area.x + 6, text_area.y + 1));
    // Releasing ends the gesture without clearing the selection.
    app.handle_mouse_event(crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Up(crossterm::event::MouseButton::Left),
        column: text_area.x + 6,
        row: text_area.y + 1,
        modifiers: crossterm::event::KeyModifiers::NONE,
    });
    assert!(app.state.editor().buffer.selection_range().is_some());

    // Wheel over the editor scrolls it.
    let before = app.state.editor().scroll;
    app.mouse_scroll(Point::new(text_area.x, text_area.y), false);
    assert_eq!(app.state.editor().scroll, before + 3);
    app.mouse_scroll(Point::new(text_area.x, text_area.y), true);
    assert_eq!(app.state.editor().scroll, before);
}

#[test]
fn hovering_a_page_row_moves_the_highlight() {
    use ordet_tui_framework::{Point, Rect};

    let mut app =
        with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
    app.state.add_draft(EditorBuffer::from_text("page"), None);
    app.state.panels.open(PanelKind::Pages);
    app.state.update_layout(Rect::new(0, 0, 120, 60));
    let area = crate::state::panel_rect(&app.state);
    let point = (area.y..area.bottom())
        .find_map(|y| {
            let point = Point::new(area.x + 2, y);
            (crate::components::panel_row_at_point(&app.state, point) == Some(0)).then_some(point)
        })
        .expect("clickable page row");
    app.mouse_hover(point);
    assert_eq!(app.state.panels.pages_selected, 0);
    // Hovering only moves the highlight: the edited page never changed.
    assert_eq!(app.state.editor().title(), "page");
}

#[test]
fn clicking_a_page_row_switches_and_closes_the_panel() {
    use ordet_tui_framework::{Point, Rect};

    let mut app =
        with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
    app.state.add_draft(EditorBuffer::from_text("one"), None);
    app.state.add_draft(EditorBuffer::from_text("two"), None);
    app.state.panels.open(PanelKind::Pages);
    app.state.update_layout(Rect::new(0, 0, 120, 60));
    let area = crate::state::panel_rect(&app.state);
    let point = (area.y..area.bottom())
        .find_map(|y| {
            let point = Point::new(area.x + 2, y);
            (crate::components::panel_row_at_point(&app.state, point) == Some(0)).then_some(point)
        })
        .expect("clickable page row");
    app.mouse_press(point);
    assert_eq!(app.state.editor().title(), "one");
    assert!(!app.state.panels.is_open());
}

#[test]
fn restoring_a_revision_brings_back_saved_text_undoably() {
    let path = unique_temp_path("ordet-revision-restore", "txt");
    fs::write(&path, "first version").expect("write test file");

    let mut app = with_test_settings(|| {
        OrdetApp::from_args(vec![path.to_string_lossy().into_owned()]).expect("app from text path")
    });

    // Save "first version" so the history has a snapshot, then keep
    // typing without saving.
    app.spawn_save_to(path.clone());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut saved = false;
    while !saved && std::time::Instant::now() < deadline {
        while let Some(task) = app.tasks.try_recv() {
            if matches!(
                task.result,
                Ok(crate::commands::AppMessage::SaveCompleted { .. })
            ) {
                saved = true;
                break;
            }
        }
        if !saved {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
    assert!(saved, "save task completed");

    app.state.editor_mut().buffer.move_cursor_to_end();
    app.state.editor_mut().buffer.insert_text(" plus unsaved");
    app.state.editor_mut().mark_dirty();
    assert_eq!(
        app.state.editor().buffer.plain_text(),
        "first version plus unsaved"
    );

    app.restore_selected_revision();
    assert_eq!(app.state.editor().buffer.plain_text(), "first version");

    // The restore went through the edit path, so one undo returns the
    // unsaved text instead of stranding it.
    assert!(app.state.editor_mut().buffer.undo());
    assert_eq!(
        app.state.editor().buffer.plain_text(),
        "first version plus unsaved"
    );
}

#[test]
fn only_unsaved_drafts_are_snapshotted_for_recovery() {
    with_test_settings(|| {
        let saved = std::env::temp_dir().join("Saved.ord");
        let dirty = std::env::temp_dir().join("Dirty.ord");
        let mut state = AppState::new(
            vec![
                (EditorBuffer::from_text("saved already"), Some(saved)),
                (EditorBuffer::from_text("still typing"), Some(dirty.clone())),
            ],
            Vec::new(),
            ThemeSelection::default(),
            None,
            12,
            Some(false),
        );

        state.switch_to_draft(1);
        state.editor_mut().buffer.move_cursor_to_end();
        state.editor_mut().buffer.insert_text(" more");
        state.editor_mut().mark_dirty();

        let snapshot = state.unsaved_drafts();

        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].title, "Dirty.ord");
        assert_eq!(snapshot[0].text, "still typing more");
        assert_eq!(
            snapshot[0].path,
            Some(crate::state::normalize_document_path(&dirty))
        );
    });
}

#[test]
fn a_scratch_draft_is_snapshotted_without_a_path() {
    with_test_settings(|| {
        let mut state = AppState::new(
            vec![(EditorBuffer::from_text("notes"), None)],
            Vec::new(),
            ThemeSelection::default(),
            None,
            12,
            Some(false),
        );
        state.editor_mut().mark_dirty();

        let snapshot = state.unsaved_drafts();

        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].path, None);
        assert_eq!(snapshot[0].text, "notes");
    });
}

#[test]
fn recovered_drafts_stay_dirty_when_the_launch_opens_files() {
    with_test_settings(|| {
        // A crashed session left one unsaved draft behind...
        let recovery = unique_temp_path("ordet-recovery-at-launch", "json");
        crate::recovery::write_recovery_to(
            &recovery,
            &crate::recovery::RecoveryFile {
                drafts: vec![crate::recovery::RecoveredDraft {
                    path: None,
                    title: "Untitled".to_string(),
                    text: "words the crash did not eat".to_string(),
                }],
            },
        )
        .expect("write recovery snapshot");
        unsafe {
            std::env::set_var("ORDET_TUI_RECOVERY_PATH", &recovery);
        }

        // ...and this launch also opens a file that is already saved.
        let opened = unique_temp_path("ordet-recovery-opened", "txt");
        fs::write(&opened, "already on disk").expect("write the opened file");

        let app = OrdetApp::from_args(vec![opened.display().to_string()]).expect("app");

        unsafe {
            std::env::remove_var("ORDET_TUI_RECOVERY_PATH");
        }

        // Only the recovered draft needs saving; the opened file does not.
        let snapshot = app.state.unsaved_drafts();
        assert_eq!(
            snapshot.len(),
            1,
            "the recovered draft must remain marked unsaved"
        );
        assert_eq!(snapshot[0].text, "words the crash did not eat");
    });
}

#[test]
fn a_lone_empty_scratch_draft_gives_way_to_recovered_work() {
    let scratch = vec![(EditorBuffer::new(), None)];
    assert!(super::is_empty_scratch(&scratch));

    let with_text = vec![(EditorBuffer::from_text("typed"), None)];
    assert!(!super::is_empty_scratch(&with_text));

    let named = vec![(EditorBuffer::new(), Some(PathBuf::from("/tmp/A.ord")))];
    assert!(!super::is_empty_scratch(&named));

    let two = vec![(EditorBuffer::new(), None), (EditorBuffer::new(), None)];
    assert!(!super::is_empty_scratch(&two));
}

#[test]
fn settings_picker_preselects_the_current_value() {
    use crate::state::SettingsPickerKind;
    use crate::theme::{ThemePreset, ThemeSelection};

    let mut app =
        with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
    app.state.ui.theme = ThemeSelection::Preset {
        preset: ThemePreset::Stalker,
    };
    app.open_settings_picker(SettingsPickerKind::Theme);
    let picker = app
        .state
        .panels
        .settings_picker
        .as_ref()
        .expect("theme picker");
    assert_eq!(picker.options.len(), ThemePreset::ALL.len());
    assert_eq!(picker.selected, 2, "Stalker is third");
    assert_eq!(picker.active, 2);

    app.state.panels.settings_picker = None;
    app.state.ui.language = crate::i18n::UiLanguage::Indonesian;
    app.open_settings_picker(SettingsPickerKind::Language);
    let picker = app
        .state
        .panels
        .settings_picker
        .as_ref()
        .expect("language picker");
    assert_eq!(picker.options.len(), 4);
    assert_eq!(picker.selected, 1);

    app.state.panels.settings_picker = None;
    app.state.ui.indent_width = 8;
    app.open_settings_picker(SettingsPickerKind::IndentWidth);
    let picker = app
        .state
        .panels
        .settings_picker
        .as_ref()
        .expect("indent picker");
    assert_eq!(
        picker.options,
        vec!["2".to_string(), "4".to_string(), "8".to_string()]
    );
    assert_eq!(picker.selected, 2);
}

#[test]
fn settings_submit_opens_a_picker_and_enter_applies() {
    use crate::state::SettingsPickerKind;
    use crate::theme::{ThemePreset, ThemeSelection};
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

    let mut app =
        with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
    app.state.add_draft(EditorBuffer::new(), None);
    app.state.ui.theme = ThemeSelection::Preset {
        preset: ThemePreset::Midnight,
    };
    app.state.panels.open(PanelKind::Settings);
    assert_eq!(app.state.panels.settings_selected, 0, "Theme row");

    // Enter on the Theme row lists instead of cycling.
    app.dispatch(UiEvent::Command(Command::Submit));
    let picker = app
        .state
        .panels
        .settings_picker
        .as_ref()
        .expect("picker opens on Submit");
    assert_eq!(picker.title, "Theme");
    assert_eq!(picker.options.len(), ThemePreset::ALL.len());
    assert_eq!(picker.selected, 0, "Midnight preselected");
    assert_eq!(
        app.state.ui.theme,
        ThemeSelection::Preset {
            preset: ThemePreset::Midnight
        },
        "opening changes nothing"
    );

    let key = |code| KeyEvent {
        code,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: crossterm::event::KeyEventState::empty(),
    };
    app.handle_key_event(key(KeyCode::Down));
    app.handle_key_event(key(KeyCode::Down));
    assert_eq!(
        app.state
            .panels
            .settings_picker
            .as_ref()
            .expect("open")
            .selected,
        2
    );
    app.handle_key_event(key(KeyCode::Enter));
    assert_eq!(app.state.panels.settings_picker, None, "Enter closes");
    assert_eq!(
        app.state.ui.theme,
        ThemeSelection::Preset {
            preset: ThemePreset::Stalker
        }
    );

    // Esc and Left close without applying.
    app.open_settings_picker(SettingsPickerKind::Language);
    app.handle_key_event(key(KeyCode::Down));
    app.handle_key_event(key(KeyCode::Esc));
    assert_eq!(app.state.panels.settings_picker, None);
    assert_eq!(app.state.ui.language, crate::i18n::UiLanguage::English);
    app.open_settings_picker(SettingsPickerKind::Language);
    app.handle_key_event(key(KeyCode::Left));
    assert_eq!(app.state.panels.settings_picker, None);
    assert_eq!(app.state.ui.language, crate::i18n::UiLanguage::English);

    // Right confirms like Enter.
    app.open_settings_picker(SettingsPickerKind::IndentWidth);
    app.handle_key_event(key(KeyCode::Right));
    assert_eq!(app.state.panels.settings_picker, None);
}

#[test]
fn settings_picker_mouse_click_confirms_and_outside_closes() {
    use ordet_tui_framework::Point;

    let mut app =
        with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
    app.state.add_draft(EditorBuffer::new(), None);
    app.state.ui.theme = crate::theme::ThemeSelection::Preset {
        preset: crate::theme::ThemePreset::Midnight,
    };
    app.state.panels.open(PanelKind::Settings);
    app.state.update_layout(Rect::new(0, 0, 120, 30));
    app.open_settings_picker(crate::state::SettingsPickerKind::Theme);

    // Hover retargets the highlight.
    let list = crate::state::settings_picker_list_rect(
        app.full_area(),
        crate::theme::ThemePreset::ALL.len(),
    );
    assert!(
        list.height > 2,
        "all {} themes fit: {list:?}",
        crate::theme::ThemePreset::ALL.len()
    );
    let hover = Point::new(list.x + 1, list.y + 5);
    app.mouse_hover(hover);
    assert_eq!(
        app.state
            .panels
            .settings_picker
            .as_ref()
            .expect("open")
            .selected,
        5
    );

    // Clicking a row applies it: Jodorowsky is eighth.
    let click = Point::new(list.x + 1, list.y + 7);
    app.mouse_press(click);
    assert_eq!(app.state.panels.settings_picker, None, "click closes");
    assert_eq!(
        app.state.ui.theme,
        crate::theme::ThemeSelection::Preset {
            preset: crate::theme::ThemePreset::Jodorowsky
        },
        "eighth row applies"
    );

    // Clicking outside closes without applying.
    app.open_settings_picker(crate::state::SettingsPickerKind::Language);
    app.mouse_press(Point::new(0, 0));
    assert_eq!(app.state.panels.settings_picker, None);
    assert_eq!(app.state.ui.language, crate::i18n::UiLanguage::English);
}

#[test]
fn settings_picker_movement_clamps_at_both_ends() {
    use crate::state::{SettingsPickerKind, SettingsPickerState};

    let mut picker = SettingsPickerState {
        kind: SettingsPickerKind::Theme,
        title: "Theme".to_string(),
        options: vec!["a".to_string(), "b".to_string(), "c".to_string()],
        selected: 0,
        scroll: 0,
        active: 0,
        preview_from: None,
    };
    picker.move_up();
    assert_eq!((picker.selected, picker.scroll), (0, 0));
    picker.move_end();
    assert_eq!((picker.selected, picker.scroll), (2, 0));
    picker.move_down();
    assert_eq!((picker.selected, picker.scroll), (2, 0));
    picker.move_home();
    assert_eq!((picker.selected, picker.scroll), (0, 0));
    // A two-row window pushes the scroll after the highlight.
    picker.selected = 2;
    picker.scroll = 0;
    picker.clamp_to(2);
    assert_eq!(picker.scroll, 1);
    picker.move_up();
    assert_eq!((picker.selected, picker.scroll), (1, 1));
}

#[test]
fn settings_picker_theme_previews_live_and_esc_restores() {
    use crate::state::SettingsPickerKind;
    use crate::theme::{ThemePreset, ThemeSelection};
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

    let mut app =
        with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
    app.state.ui.theme = ThemeSelection::Preset {
        preset: ThemePreset::Midnight,
    };
    app.open_settings_picker(SettingsPickerKind::Theme);
    assert!(app.state.panels.settings_picker.is_some());

    let key = |code| KeyEvent {
        code,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: crossterm::event::KeyEventState::empty(),
    };
    // Browsing applies immediately, without persisting or closing.
    app.handle_key_event(key(KeyCode::Down));
    assert_eq!(
        app.state.ui.theme,
        ThemeSelection::Preset {
            preset: ThemePreset::Ordet
        },
        "highlight previews live"
    );
    assert!(app.state.panels.settings_picker.is_some());

    // Esc restores whatever was in force and closes.
    app.handle_key_event(key(KeyCode::Down));
    assert_eq!(
        app.state.ui.theme,
        ThemeSelection::Preset {
            preset: ThemePreset::Stalker
        }
    );
    app.handle_key_event(key(KeyCode::Esc));
    assert_eq!(app.state.panels.settings_picker, None);
    assert_eq!(
        app.state.ui.theme,
        ThemeSelection::Preset {
            preset: ThemePreset::Midnight
        },
        "Esc restores the pre-browse theme"
    );

    // Enter keeps the previewed theme and closes.
    app.open_settings_picker(SettingsPickerKind::Theme);
    app.handle_key_event(key(KeyCode::Down));
    app.handle_key_event(key(KeyCode::Enter));
    assert_eq!(app.state.panels.settings_picker, None);
    assert_eq!(
        app.state.ui.theme,
        ThemeSelection::Preset {
            preset: ThemePreset::Ordet
        }
    );
}

#[test]
fn settings_picker_only_themes_preview_live() {
    use crate::state::SettingsPickerKind;
    use crate::theme::{ThemePreset, ThemeSelection};

    let mut app =
        with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
    app.state.ui.theme = ThemeSelection::Preset {
        preset: ThemePreset::Stalker,
    };
    app.state.ui.language = crate::i18n::UiLanguage::Indonesian;
    app.open_settings_picker(SettingsPickerKind::Language);
    app.state
        .panels
        .settings_picker
        .as_mut()
        .expect("open")
        .selected = 2;
    app.preview_settings_picker_theme();
    assert_eq!(
        app.state.ui.theme,
        ThemeSelection::Preset {
            preset: ThemePreset::Stalker
        },
        "other kinds never touch the theme"
    );
}

#[test]
fn test_menu_catalog_is_fully_wired() {
    use crate::commands::{MenuEffect, MENU_BAR};

    assert!(!MENU_BAR.is_empty());
    for menu in MENU_BAR {
        assert!(!menu.title.is_empty());
        assert!(!menu.items.is_empty());
        for item in menu.items {
            assert!(!item.label.is_empty());
            match &item.effect {
                MenuEffect::Action(_) | MenuEffect::Command(_) => {}
            }
        }
    }
    // Spot-check titles the header paints.
    let titles: Vec<&str> = MENU_BAR.iter().map(|m| m.title).collect();
    assert_eq!(
        titles,
        vec!["File", "Edit", "View", "Insert", "Format", "Page", "Window", "Help"]
    );
}

#[test]
fn window_menu_only_lists_windows_that_exist() {
    use crate::commands::{menu_items, MENU_BAR};

    let window = MENU_BAR
        .iter()
        .position(|menu| menu.title == "Window")
        .expect("Window menu");
    let labels = |windows: usize| {
        menu_items(window, windows)
            .into_iter()
            .map(|item| item.label)
            .collect::<Vec<_>>()
    };

    assert_eq!(labels(1), vec!["Next window"]);
    assert_eq!(
        labels(2),
        vec![
            "Next window",
            "Close window",
            "Move to slot 1",
            "Move to slot 2",
        ]
    );
    assert_eq!(labels(3).len(), 5);
    assert_eq!(labels(4).len(), 6);
}

#[test]
fn test_menu_keys_open_navigate_run_and_close() {
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    let mut app =
        with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
    // A second draft leaves the welcome screen so menu keys route.
    app.state
        .add_draft(crate::editor_buffer::EditorBuffer::new(), None);
    assert!(!app.state.show_welcome_screen());

    let key = |code: KeyCode| KeyEvent {
        code,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::empty(),
    };

    // F10 opens the File menu. The menu explains itself through its rows;
    // it does not take over the status line with a hint.
    app.handle_key_event(key(KeyCode::F(10)));
    assert_eq!(app.state.ui.menu_open, Some(0));
    assert_eq!(app.state.ui.menu_selected, 0);
    assert!(
        app.state.ui.status.is_empty(),
        "status: {}",
        app.state.ui.status
    );

    // Right switches to Edit, Down/Up move, End clamps to the last item.
    app.handle_key_event(key(KeyCode::Right));
    assert_eq!(app.state.ui.menu_open, Some(1));
    assert_eq!(app.state.ui.menu_selected, 0);
    app.handle_key_event(key(KeyCode::Down));
    app.handle_key_event(key(KeyCode::Down));
    assert_eq!(app.state.ui.menu_selected, 2);
    app.handle_key_event(key(KeyCode::Up));
    assert_eq!(app.state.ui.menu_selected, 1);
    app.handle_key_event(key(KeyCode::Left));
    assert_eq!(app.state.ui.menu_open, Some(0));

    // Esc closes without running anything.
    let drafts = app.state.drafts.len();
    app.handle_key_event(key(KeyCode::Esc));
    assert!(app.state.ui.menu_open.is_none());
    assert_eq!(app.state.drafts.len(), drafts);

    // Enter on File → New draft adds a draft and closes the menu.
    app.handle_key_event(key(KeyCode::F(10)));
    app.handle_key_event(key(KeyCode::Enter));
    assert!(app.state.ui.menu_open.is_none());
    assert_eq!(app.state.drafts.len(), drafts + 1);

    // F10 toggles an open menu closed.
    app.handle_key_event(key(KeyCode::F(10)));
    assert!(app.state.ui.menu_open.is_some());
    app.handle_key_event(key(KeyCode::F(10)));
    assert!(app.state.ui.menu_open.is_none());
}

#[test]
fn test_menu_mouse_opens_runs_and_dismisses() {
    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

    let mut app =
        with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
    app.state
        .add_draft(crate::editor_buffer::EditorBuffer::new(), None);
    app.state.ui.layout.header = ordet_tui_framework::Rect::new(0, 0, 80, 1);
    app.state.ui.layout.status = ordet_tui_framework::Rect::new(0, 20, 80, 2);

    let click = |column: u16, row: u16| MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::NONE,
    };
    let release = |column: u16, row: u16| MouseEvent {
        kind: MouseEventKind::Up(MouseButton::Left),
        column,
        row,
        modifiers: KeyModifiers::NONE,
    };
    let hover = |column: u16, row: u16| MouseEvent {
        kind: MouseEventKind::Moved,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    };

    // Click the File title: cells start at x=0, "File" is 4 wide.
    let cells = crate::state::menu_title_cells(app.state.ui.layout.header);
    assert!(!cells.is_empty());
    app.handle_mouse_event(click(cells[0].x + 1, 0));
    assert_eq!(app.state.ui.menu_open, Some(0));

    // Hover traces the highlight without running anything. Rows live
    // inside the border, so hit-test the inner list rect.
    let inner = crate::state::menu_drop_inner_rect(
        app.state.ui.layout.header,
        app.state.ui.layout.status.y,
        0,
        crate::commands::MENU_BAR[0].items.len(),
    );
    assert!(inner.height > 0);
    app.handle_mouse_event(hover(inner.x + 1, inner.y + 2));
    assert_eq!(app.state.ui.menu_open, Some(0));
    assert_eq!(app.state.ui.menu_selected, 2);

    // Press arms, release on the row runs: File → New draft.
    let drafts = app.state.drafts.len();
    app.handle_mouse_event(click(inner.x + 1, inner.y));
    assert_eq!(app.state.ui.menu_open, Some(0));
    assert_eq!(app.state.drafts.len(), drafts);
    app.handle_mouse_event(release(inner.x + 1, inner.y));
    assert!(app.state.ui.menu_open.is_none());
    assert_eq!(app.state.drafts.len(), drafts + 1);

    // Clicking outside an open menu just closes it.
    app.handle_mouse_event(click(cells[1].x + 1, 0));
    assert_eq!(app.state.ui.menu_open, Some(1));
    app.handle_mouse_event(click(79, 10));
    assert!(app.state.ui.menu_open.is_none());
    app.handle_mouse_event(release(79, 10));
    assert!(app.state.ui.menu_open.is_none());

    // Press-drag-release across rows runs the release row (here Open
    // file, which opens the browser, so it goes last).
    app.handle_mouse_event(click(cells[0].x + 1, 0));
    assert_eq!(app.state.ui.menu_open, Some(0));
    let inner = crate::state::menu_drop_inner_rect(
        app.state.ui.layout.header,
        app.state.ui.layout.status.y,
        0,
        crate::commands::MENU_BAR[0].items.len(),
    );
    app.handle_mouse_event(click(inner.x + 1, inner.y));
    app.handle_mouse_event(hover(inner.x + 1, inner.y + 1));
    app.handle_mouse_event(release(inner.x + 1, inner.y + 1));
    assert!(app.state.ui.menu_open.is_none());
}

#[test]
fn test_bookmarks_set_and_jump_through_ctrl_k() {
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    let mut app =
        with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
    app.state
        .add_draft(crate::editor_buffer::EditorBuffer::new(), None);
    // A real editor rect so visual-line movement is not degenerate.
    app.state.ui.layout.editor = ordet_tui_framework::Rect::new(0, 0, 80, 24);

    let key = |code: KeyCode, modifiers: KeyModifiers| KeyEvent {
        code,
        modifiers,
        kind: KeyEventKind::Press,
        state: KeyEventState::empty(),
    };
    let ctrl_k = || key(KeyCode::Char('k'), KeyModifiers::CONTROL);

    // Type some text; the cursor ends at 11.
    for ch in "hello world".chars() {
        app.handle_key_event(key(KeyCode::Char(ch), KeyModifiers::NONE));
    }
    assert_eq!(app.state.editor().buffer.cursor(), 11);

    // Ctrl-K 3 drops bookmark 3 at the cursor.
    app.handle_key_event(ctrl_k());
    app.handle_key_event(key(KeyCode::Char('3'), KeyModifiers::NONE));
    assert_eq!(app.state.editor().bookmarks[3], Some(11));

    app.handle_key_event(key(KeyCode::Home, KeyModifiers::NONE));
    assert_eq!(app.state.editor().buffer.cursor(), 0);
    app.handle_key_event(ctrl_k());
    app.handle_key_event(key(KeyCode::Char('q'), KeyModifiers::NONE));
    app.handle_key_event(key(KeyCode::Char('3'), KeyModifiers::NONE));
    assert_eq!(app.state.editor().buffer.cursor(), 11);

    // Jumping to an empty bookmark reports it and stays put.
    app.handle_key_event(ctrl_k());
    app.handle_key_event(key(KeyCode::Char('q'), KeyModifiers::NONE));
    app.handle_key_event(key(KeyCode::Char('7'), KeyModifiers::NONE));
    assert_eq!(app.state.editor().buffer.cursor(), 11);
    assert!(app.state.editor().bookmarks[7].is_none());
}

#[test]
fn test_quick_bookmarks_need_no_slot_numbers() {
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    let mut app =
        with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
    app.state
        .add_draft(crate::editor_buffer::EditorBuffer::new(), None);
    app.state.ui.layout.editor = ordet_tui_framework::Rect::new(0, 0, 80, 24);

    let key = |code: KeyCode, modifiers: KeyModifiers| KeyEvent {
        code,
        modifiers,
        kind: KeyEventKind::Press,
        state: KeyEventState::empty(),
    };

    // No marks yet: jumping to the last one reports it and stays put.
    app.apply_action(crate::commands::Action::JumpLastBookmark);
    assert_eq!(app.state.editor().buffer.cursor(), 0);

    for ch in "hello world".chars() {
        app.handle_key_event(key(KeyCode::Char(ch), KeyModifiers::NONE));
    }
    // Dropping picks the lowest free slot on its own.
    app.apply_action(crate::commands::Action::DropQuickBookmark);
    assert_eq!(app.state.editor().bookmarks[0], Some(11));
    app.handle_key_event(key(KeyCode::Home, KeyModifiers::NONE));
    app.apply_action(crate::commands::Action::DropQuickBookmark);
    assert_eq!(app.state.editor().bookmarks[1], Some(0));

    // The menu-free chord way: Ctrl-K Q Enter jumps to the last mark.
    app.handle_key_event(key(KeyCode::End, KeyModifiers::NONE));
    assert_eq!(app.state.editor().buffer.cursor(), 11);
    app.handle_key_event(key(KeyCode::Char('k'), KeyModifiers::CONTROL));
    app.handle_key_event(key(KeyCode::Char('q'), KeyModifiers::NONE));
    app.handle_key_event(key(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(app.state.editor().buffer.cursor(), 0);
}

#[test]
fn test_alt_bold_italic_style_the_selection() {
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    let mut app =
        with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
    app.state
        .add_draft(crate::editor_buffer::EditorBuffer::new(), None);

    let key = |code: KeyCode, modifiers: KeyModifiers| KeyEvent {
        code,
        modifiers,
        kind: KeyEventKind::Press,
        state: KeyEventState::empty(),
    };
    for ch in "hello".chars() {
        app.handle_key_event(key(KeyCode::Char(ch), KeyModifiers::NONE));
    }
    // Select "llo" with Shift+Left x3, then Alt-B bolds it.
    for _ in 0..3 {
        app.handle_key_event(key(KeyCode::Left, KeyModifiers::SHIFT));
    }
    app.handle_key_event(key(KeyCode::Char('b'), KeyModifiers::ALT));
    let bolds: Vec<(usize, usize)> = app
        .state
        .editor()
        .buffer
        .attr_runs_in(0, 5)
        .into_iter()
        .filter(|(_, _, bold, _)| *bold)
        .map(|(s, e, _, _)| (s, e))
        .collect();
    assert_eq!(bolds, vec![(2, 5)]);

    // Alt-E italicizes the same range on top; Alt-B again unbolds.
    app.handle_key_event(key(KeyCode::Char('e'), KeyModifiers::ALT));
    app.handle_key_event(key(KeyCode::Char('b'), KeyModifiers::ALT));
    let runs = app.state.editor().buffer.attr_runs_in(0, 5);
    assert_eq!(runs, vec![(0, 2, false, false), (2, 5, false, true)]);
}

#[test]
fn test_repeated_up_keys_never_move_the_cursor_down() {
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    let mut app =
        with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
    app.state
        .add_draft(crate::editor_buffer::EditorBuffer::new(), None);
    app.state.ui.layout.editor = ordet_tui_framework::Rect::new(0, 0, 80, 24);

    let text = (0..500)
        .map(|i| format!("line {i:03} with some words to wrap around"))
        .collect::<Vec<_>>()
        .join("\n");
    app.state.editor_mut().buffer = crate::editor_buffer::EditorBuffer::from_text(&text);
    app.state.editor_mut().buffer.move_cursor_to_end();

    let key_up = KeyEvent {
        code: KeyCode::Up,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::empty(),
    };
    let mut previous = app.state.editor().buffer.cursor();
    for _ in 0..600 {
        app.handle_key_event(key_up);
        let cursor = app.state.editor().buffer.cursor();
        assert!(
            cursor <= previous,
            "cursor snapped down from {previous} to {cursor}"
        );
        previous = cursor;
    }
}

#[test]
fn test_block_quote_command_marks_the_current_line() {
    use crate::commands::Command;
    use ordet_tui_framework::UiEvent;

    let mut app =
        with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
    app.state
        .add_draft(crate::editor_buffer::EditorBuffer::new(), None);

    for ch in "wise words".chars() {
        app.state.editor_mut().buffer.insert_text(&ch.to_string());
    }
    app.dispatch(UiEvent::Command(Command::BlockQuote));
    assert_eq!(app.state.editor().buffer.plain_text(), "> wise words");
    app.dispatch(UiEvent::Command(Command::BlockBody));
    assert_eq!(app.state.editor().buffer.plain_text(), "wise words");
}

#[test]
fn test_welcome_opening_menu_letters() {
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    let mut app =
        with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
    assert!(app.state.show_welcome_screen());

    let key = |code: KeyCode| KeyEvent {
        code,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::empty(),
    };

    // D opens a draft: the scratch is replaced and the
    // welcome screen leaves at once.
    app.handle_key_event(key(KeyCode::Char('d')));
    assert_eq!(app.state.drafts.len(), 1);
    assert!(!app.state.show_welcome_screen());
}

#[test]
fn test_welcome_o_hotkey_selects_open_and_browses() {
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    let mut app =
        with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
    assert!(app.state.show_welcome_screen());

    let key = |code: KeyCode| KeyEvent {
        code,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::empty(),
    };

    // O opens the browser and moves the highlight along with it.
    app.handle_key_event(key(KeyCode::Char('o')));
    assert!(app.state.ui.file_browser.is_some());
    assert_eq!(
        app.state.ui.welcome_selected,
        crate::state::WelcomeOption::OpenFile
    );
    assert!(app.state.show_welcome_screen());

    // H then jumps to help: highlight follows, the browser dismisses
    // so the shortcuts rail actually shows.
    app.handle_key_event(key(KeyCode::Char('h')));
    assert_eq!(
        app.state.ui.welcome_selected,
        crate::state::WelcomeOption::Help
    );
    assert!(app.state.welcome_help_active());
    assert!(app.state.ui.file_browser.is_none());
    assert!(app.state.show_welcome_screen());
}

#[test]
fn test_welcome_browser_search_mode_keeps_typing() {
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    let mut app =
        with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
    assert!(app.state.show_welcome_screen());

    let key = |code: KeyCode| KeyEvent {
        code,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::empty(),
    };

    // Open the browser rail, then Tab into search mode.
    app.handle_key_event(key(KeyCode::Char('o')));
    assert!(app.state.ui.file_browser.is_some());
    app.handle_key_event(key(KeyCode::Tab));
    assert!(matches!(
        app.state
            .ui
            .file_browser
            .as_ref()
            .map(|browser| browser.mode),
        Some(crate::file_browser::FileBrowserMode::Search)
    ));

    // Typing now filters instead of firing menu hotkeys.
    app.handle_key_event(key(KeyCode::Char('h')));
    let browser = app.state.ui.file_browser.as_ref().expect("browser stays");
    assert!(browser.search.contains('h'));
    assert_eq!(
        app.state.ui.welcome_selected,
        crate::state::WelcomeOption::OpenFile
    );
}

#[test]
fn test_welcome_h_hotkey_opens_help_without_enter() {
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    let mut app =
        with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
    assert!(app.state.show_welcome_screen());

    app.handle_key_event(KeyEvent {
        code: KeyCode::Char('h'),
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::empty(),
    });
    assert_eq!(
        app.state.ui.welcome_selected,
        crate::state::WelcomeOption::Help
    );
    assert!(app.state.welcome_help_active());
}

#[test]
fn test_welcome_x_quits() {
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    let mut app =
        with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
    assert!(app.state.show_welcome_screen());

    app.handle_key_event(KeyEvent {
        code: KeyCode::Char('x'),
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::empty(),
    });
    assert!(app.state.ui.should_quit);
}

fn autosave_temp_path(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("ordet-autosave-test-{}-{name}", std::process::id()))
}

#[test]
fn test_flush_writes_dirty_drafts_to_disk() {
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    let mut app =
        with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
    let path = autosave_temp_path("flush.txt");
    let _ = std::fs::remove_file(&path);
    app.state.add_draft(
        crate::editor_buffer::EditorBuffer::new(),
        Some(path.clone()),
    );
    // Type a word: the draft is dirty and the file does not exist yet.
    app.handle_key_event(KeyEvent {
        code: KeyCode::Char('a'),
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::empty(),
    });
    assert!(app.state.editor().is_dirty());
    assert!(!path.exists());

    app.flush_dirty_drafts().expect("flush writes");
    assert!(!app.state.editor().is_dirty());
    assert_eq!(std::fs::read_to_string(&path).expect("saved file"), "a");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_quit_saves_and_leaves_without_asking() {
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    let mut app =
        with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
    let path = autosave_temp_path("quit.txt");
    let _ = std::fs::remove_file(&path);
    app.state.add_draft(
        crate::editor_buffer::EditorBuffer::new(),
        Some(path.clone()),
    );
    app.handle_key_event(KeyEvent {
        code: KeyCode::Char('b'),
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::empty(),
    });

    app.request_quit();
    assert!(app.state.ui.should_quit);
    assert!(app.state.ui.quit_confirmation.is_none());
    assert_eq!(std::fs::read_to_string(&path).expect("saved file"), "b");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_quit_asks_only_when_the_write_fails() {
    let mut app =
        with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
    // A directory is not writable as a file: the flush must fail.
    let dir = std::env::temp_dir().join(format!("ordet-autosave-test-{}-dir", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    app.state
        .add_draft(crate::editor_buffer::EditorBuffer::new(), Some(dir.clone()));
    app.state.editor_mut().buffer.insert_text("x");
    app.state.editor_mut().mark_dirty();

    app.request_quit();
    assert!(!app.state.ui.should_quit);
    assert!(app.state.ui.quit_confirmation.is_some());
    let _ = std::fs::remove_dir(&dir);
}

#[test]
fn test_autosave_tick_saves_dirty_drafts_quietly() {
    let mut app =
        with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
    // Clean scratch first: nothing to do, no task spawns.
    app.next_autosave_at = std::time::Instant::now() - std::time::Duration::from_secs(10);
    app.maybe_autosave();
    assert!(!app.state.editor().pending_save);

    let path = autosave_temp_path("tick.txt");
    let _ = std::fs::remove_file(&path);
    app.state.add_draft(
        crate::editor_buffer::EditorBuffer::new(),
        Some(path.clone()),
    );
    app.state.editor_mut().buffer.insert_text("z");
    app.state.editor_mut().mark_dirty();

    // The tick schedules a background save...
    app.next_autosave_at = std::time::Instant::now() - std::time::Duration::from_secs(10);
    app.maybe_autosave();
    assert!(app.state.editor().pending_save);

    // ...whose completion lands quietly: no status chatter, just a
    // clean draft and a current file.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while app.state.editor().is_dirty() {
        assert!(
            std::time::Instant::now() < deadline,
            "autosave task never completed"
        );
        while let Some(task) = app.tasks.try_recv() {
            app.handle_task_result(task);
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[test]
fn test_status_paths_show_parent_and_file_only() {
    use std::path::Path;

    assert_eq!(
        OrdetApp::display_path_tail(Path::new("/Users/leon/Documents/Ordet-final/i.ord")),
        "Ordet-final/i.ord"
    );
    assert_eq!(
        OrdetApp::display_path_tail(Path::new("/notes.txt")),
        "notes.txt"
    );
    assert_eq!(OrdetApp::display_path_tail(Path::new("/")), "/");
}

#[test]
fn test_status_messages_expire_back_to_the_default_hint() {
    use ordet_tui_framework::UiEvent;

    let mut app =
        with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
    app.state
        .add_draft(crate::editor_buffer::EditorBuffer::new(), None);

    // Transient messages arm the timer.
    app.state
        .set_status("Opened Ordet-final/i.ord.".to_string());
    assert!(app.state.ui.status_expires_at.is_some());

    // Persistent ones never do.
    app.state
        .set_status_persistent("A persistent note that never expires.".to_string());
    assert!(app.state.ui.status_expires_at.is_none());

    // A past deadline clears the message on tick: an idle editor keeps
    // the bar empty and the cluster alone carries the state.
    app.state
        .set_status("Opened Ordet-final/i.ord.".to_string());
    app.state.ui.status_expires_at =
        Some(std::time::Instant::now() - std::time::Duration::from_secs(10));
    app.dispatch(UiEvent::Tick);
    assert!(app.state.ui.status_expires_at.is_none());
    assert!(
        app.state.ui.status.is_empty(),
        "unexpected status: {}",
        app.state.ui.status
    );
}

#[test]
fn routine_status_is_silent_and_keeps_persistent_prompts() {
    let mut app =
        with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
    app.state
        .add_draft(crate::editor_buffer::EditorBuffer::new(), None);

    // A routine success clears an old transient note instead of showing
    // another one.
    app.state.set_status("No matches.".to_string());
    app.state.set_status_quiet("Saved.".to_string());
    assert!(app.state.ui.status.is_empty());
    assert!(app.state.ui.status_expires_at.is_none());

    // Persistent confirmations and modal hints survive routine actions.
    app.state
        .set_status_persistent("Delete this page? Y/N".to_string());
    app.state.set_status_quiet("Saved.".to_string());
    assert_eq!(app.state.ui.status, "Delete this page? Y/N");
    assert!(app.state.ui.status_expires_at.is_none());
}

#[test]
fn test_wheel_scroll_stays_where_put_until_cursor_moves() {
    use ordet_tui_framework::Point;

    let mut app =
        with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
    app.state
        .add_draft(crate::editor_buffer::EditorBuffer::new(), None);
    app.state.ui.layout.editor = ordet_tui_framework::Rect::new(0, 1, 80, 20);

    let text = (0..100)
        .map(|i| format!("line {i:03}"))
        .collect::<Vec<_>>()
        .join("\n");
    app.state.editor_mut().buffer = crate::editor_buffer::EditorBuffer::from_text(&text);
    assert_eq!(app.state.editor().buffer.cursor(), 0);

    // Trackpad fling down: the view moves and stays moved.
    for _ in 0..3 {
        app.mouse_scroll(Point::new(5, 5), false);
    }
    assert_eq!(app.state.editor().scroll, 9);
    app.state.editor_mut().ensure_cursor_visible(70, 20);
    assert_eq!(
        app.state.editor().scroll,
        9,
        "free scroll must survive the per-frame ensure"
    );

    // Moving the cursor re-pins the view to it.
    app.state.editor_mut().buffer.move_down(70, false);
    app.state.editor_mut().ensure_cursor_visible(70, 20);
    assert!(app.state.editor().free_scroll_at.is_none());
    assert_eq!(app.state.editor().scroll, 1);
}

#[test]
fn about_menu_opens_a_sheet_and_never_writes_a_status_note() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let mut app =
        with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
    app.state.set_status("stale note".to_string());

    // Help ▸ About Ordet: after Keyboard help in the last menu.
    app.run_menu_item(7, 1);
    assert!(app.state.ui.about_open);
    assert!(
        app.state.ui.status.is_empty(),
        "About must not announce itself in the status bar: {}",
        app.state.ui.status
    );
    assert!(app.state.ui.status_expires_at.is_none());

    app.handle_key_event(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(!app.state.ui.about_open);
}

#[test]
fn test_notes_wheel_scroll_stays_until_cursor_moves() {
    use ordet_tui_framework::Point;

    let mut app =
        with_test_settings(|| OrdetApp::from_args(Vec::new()).expect("app with scratch draft"));
    app.state
        .add_draft(crate::editor_buffer::EditorBuffer::new(), None);
    app.state
        .update_layout(ordet_tui_framework::Rect::new(0, 0, 120, 30));
    app.state.panels.open(PanelKind::Notes);

    let text = (0..100)
        .map(|i| format!("note {i:03}"))
        .collect::<Vec<_>>()
        .join("\n");
    app.state.notes_mut().buffer = crate::editor_buffer::EditorBuffer::from_text(&text);

    app.mouse_scroll(Point::new(85, 5), false);
    assert_eq!(app.state.notes().scroll, 3);
    app.state.notes_mut().ensure_cursor_visible(30, 20);
    assert_eq!(app.state.notes().scroll, 3);

    app.state.notes_mut().buffer.move_down(30, false);
    app.state.notes_mut().ensure_cursor_visible(30, 20);
    assert!(app.state.notes().free_scroll_at.is_none());
    assert_eq!(app.state.notes().scroll, 1);
}
