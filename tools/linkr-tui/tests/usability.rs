//! Task-level UI regressions: editing, reading, navigation and small terminals.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use linkr_tui::{
    app::{App, Focus},
    event::View,
    i18n::Lang,
    ui,
    views::assistant,
};
use ratatui::{layout::Rect, text::Line};

fn rendered(lines: &[Line<'_>]) -> String {
    lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn conversation() -> App {
    let mut app = App {
        view: View::Assistant,
        screen: Rect::new(0, 0, 80, 24),
        ..App::default()
    };
    app.assistant.configured = true;
    app.assistant
        .chat_mut()
        .display
        .push(linkr_tui::agent::session::DisplayLine {
            role: "assistant".into(),
            text: (1..=60)
                .map(|i| format!("row {i:02}"))
                .collect::<Vec<_>>()
                .join("\n"),
        });
    app.assistant.chat_mut().composer = "draft stays visible".into();
    app
}

#[test]
fn long_reply_follows_latest_and_input_is_anchored() {
    let app = conversation();
    let lines = assistant::full_lines(&app, &app.theme(), 48, 20);
    assert_eq!(lines.len(), 20);
    let text = rendered(&lines);
    assert!(text.contains("row 60"), "{text}");
    assert!(!text.contains("row 01"), "{text}");
    assert!(rendered(&lines[18..19]).contains("draft stays visible"));
}

#[test]
fn reading_history_keeps_its_position_when_a_reply_arrives() {
    let mut app = conversation();
    let path = std::env::temp_dir().join(format!("bee-reading-{}.json", uuid::Uuid::new_v4()));
    app.agent_store = Some(linkr_tui::agent::store::JsonFile::at(&path));
    assistant::scroll_in(&mut app, -10, 48, 20);
    assistant::handle_key(&mut app, KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
    let cursor = app.assistant.chat().composer_cursor;
    let before = assistant::full_lines(&app, &app.theme(), 48, 20);
    assert!(!app.assistant.chat().scroll.follow);
    app.assistant
        .chat_mut()
        .display
        .push(linkr_tui::agent::session::DisplayLine {
            role: "assistant".into(),
            text: "newest reply".into(),
        });
    assistant::persist_session(&mut app);
    let after = assistant::full_lines(&app, &app.theme(), 48, 20);
    assert_eq!(rendered(&before[2..17]), rendered(&after[2..17]));
    assert_eq!(app.assistant.chat().composer_cursor, cursor);
    assistant::scroll_in(&mut app, isize::MAX, 48, 20);
    assert!(app.assistant.chat().scroll.follow);
    assert!(rendered(&assistant::full_lines(&app, &app.theme(), 48, 20)).contains("newest reply"));
    std::fs::remove_file(path).unwrap();
}

#[test]
fn saving_keeps_reading_positions_with_their_own_conversations() {
    let mut app = conversation();
    let path = std::env::temp_dir().join(format!("bee-positions-{}.json", uuid::Uuid::new_v4()));
    app.agent_store = Some(linkr_tui::agent::store::JsonFile::at(&path));
    assistant::scroll_in(&mut app, -10, 48, 20);
    let history = app.assistant.chat().scroll;
    let first_id = app.assistant.chat().id;
    app.assistant.chats.push(assistant::ChatState {
        id: first_id + 1,
        display: app.assistant.chat().display.clone(),
        ..assistant::ChatState::default()
    });
    app.assistant.active = 1;
    assistant::persist_session(&mut app);
    assert!(app.assistant.chat().scroll.follow);
    assert_eq!(app.assistant.chats[0].scroll, history);
    app.assistant.active = 0;
    assert_eq!(app.assistant.chat().scroll, history);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn full_view_and_panel_use_identical_reading_and_copy_rows() {
    let app = conversation();
    let area = Rect::new(0, 0, 48, 20);
    let panel = assistant::panel_lines(&app, &app.theme(), area.width, area.height);
    let full = assistant::full_lines(&app, &app.theme(), area.width, area.height);
    assert_eq!(rendered(&panel), rendered(&full));
    let copied = assistant::painted_rows(&app, assistant::SelectRegion::View, area).join("\n");
    assert_eq!(copied, rendered(&full));
}

#[test]
fn composer_edits_in_the_middle_and_pastes_at_the_caret() {
    let mut app = conversation();
    app.assistant.chat_mut().composer = "abc".into();
    assistant::handle_key(&mut app, KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
    assistant::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('X'), KeyModifiers::NONE),
    );
    assistant::paste_at_caret(&mut app, "中文");
    assert_eq!(app.assistant.chat().composer, "abX中文c");
    assistant::handle_key(&mut app, KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE));
    assert_eq!(app.assistant.chat().composer, "abX中文");
}

#[test]
fn long_draft_shows_the_insertion_point_instead_of_a_frozen_prefix() {
    let mut app = conversation();
    app.assistant.chat_mut().composer = format!("{}TAIL", "中文".repeat(40));
    let text = rendered(&assistant::full_lines(&app, &app.theme(), 30, 18));
    assert!(text.lines().nth(16).unwrap().contains("TAIL"), "{text}");
    assistant::handle_key(&mut app, KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
    let text = rendered(&assistant::full_lines(&app, &app.theme(), 30, 18));
    assert!(!text.lines().nth(16).unwrap().contains("TAIL"));
}

#[test]
fn model_settings_use_the_same_editor_without_revealing_a_pasted_key() {
    let mut app = conversation();
    assistant::open_settings(&mut app, assistant::AgentField::Endpoint);
    app.assistant.endpoint = "http://localhost/v1".into();
    assistant::handle_settings_key(&mut app, KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
    assistant::paste_at_caret(&mut app, "X");
    assert_eq!(app.assistant.endpoint, "Xhttp://localhost/v1");
    assistant::open_settings(&mut app, assistant::AgentField::ApiKey);
    assistant::paste_at_caret(&mut app, "secret-key");
    let text = rendered(&assistant::settings_lines(&app, &app.theme(), 25));
    assert!(!text.contains("secret-key"), "{text}");
    for code in [KeyCode::Home, KeyCode::Right, KeyCode::Char('X')] {
        assistant::handle_settings_key(&mut app, KeyEvent::new(code, KeyModifiers::NONE));
    }
    assert_eq!(app.assistant.api_key, "sXecret-key");
    let text = rendered(&assistant::settings_lines(&app, &app.theme(), 25));
    assert!(text.contains("*X*********"), "{text}");
}

#[test]
fn editing_chords_pass_through_the_real_settings_dialog_dispatch() {
    let mut app = conversation();
    assistant::open_settings(&mut app, assistant::AgentField::Endpoint);
    app.assistant.endpoint = "http://localhost/v1".into();
    assert!(linkr_tui::dialogs::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL)
    ));
    assistant::paste_at_caret(&mut app, "X");
    assert_eq!(app.assistant.endpoint, "Xhttp://localhost/v1");
    assert!(linkr_tui::dialogs::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL)
    ));
    assert!(linkr_tui::dialogs::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL)
    ));
    assert!(app.assistant.endpoint.is_empty());
    assert!(app.dialog.is_some(), "editing must not dismiss the form");
    app.assistant.endpoint = "http://localhost/v1".into();
    app.assistant.model = "local-model".into();
    app.assistant.focus = assistant::AgentField::Notes;
    app.assistant.note_input = "unsaved note".into();
    assert!(linkr_tui::dialogs::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL)
    ));
    assert!(
        app.assistant.config.is_some(),
        "Ctrl+S saves the model from any field"
    );
    assert_eq!(app.assistant.note_input, "unsaved note");
    assert!(app.assistant.notes.is_empty());
}

#[test]
fn fresh_bluetooth_connection_chooses_a_board_but_saved_and_lan_links_do_not() {
    use linkr_tui::{app::Transport, session::Dial, transport::ble::Selection};
    let mut app = App::default();
    assert!(app.needs_device_choice());
    app.last_dial = Some(Dial::Ble(
        Selection::Address("AA:BB:CC:DD:EE:FF".into()),
        None,
    ));
    assert!(!app.needs_device_choice());
    app.transport = Transport::Lan;
    app.last_dial = None;
    assert!(!app.needs_device_choice());
}

#[test]
fn history_opens_without_replacing_the_conversation_or_task_evidence() {
    let mut app = conversation();
    app.assistant
        .tasks
        .push(linkr_tui::agent::tasks::TaskRecord {
            id: "task".into(),
            device_key: "board".into(),
            updated_at: 1,
            goal: "historic operation".into(),
            summary: "task evidence".into(),
            status: "done".into(),
        });
    assistant::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL),
    );
    assert!(matches!(
        app.dialog,
        Some(linkr_tui::dialogs::Dialog::ChatPick(_))
    ));
    assert_eq!(app.assistant.tasks[0].summary, "task evidence");
    linkr_tui::dialogs::handle_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(rendered(&assistant::full_lines(&app, &app.theme(), 48, 20)).contains("row 60"));
}

#[test]
fn small_windows_keep_content_wide_and_offer_a_temporary_drawer() {
    let mut app = App {
        screen: Rect::new(0, 0, 60, 20),
        ..App::default()
    };
    let regions = ui::regions_for(app.screen, true, true);
    assert_eq!(regions.sidebar.width, 0);
    assert_eq!(regions.centre.width, 60);
    assert!(ui::sidebar_drawer(&app, app.screen).is_none());
    app.focus = Focus::Sidebar;
    assert_eq!(ui::sidebar_drawer(&app, app.screen).unwrap().width, 30);
    assert_eq!(ui::regions_for(app.screen, true, true).centre.width, 60);
    app.focus = Focus::Terminal;
    assert!(ui::sidebar_drawer(&app, app.screen).is_none());
}

#[test]
fn sidebar_selection_stays_visible_at_short_heights() {
    let mut app = App {
        focus: Focus::Sidebar,
        ..App::default()
    };
    app.quick_send
        .push(linkr_tui::app::QuickSend::guarded("reboot"));
    for _ in 0..40 {
        app.cursor_down();
    }
    let text = rendered(&ui::sidebar::visible_lines(&app, &app.theme(), 29, 10));
    assert!(text.contains("reboot !"), "{text}");
    assert!(text.contains(app.theme().glyphs().cursor()), "{text}");
    for _ in 0..40 {
        app.cursor_up();
    }
    let text = rendered(&ui::sidebar::visible_lines(&app, &app.theme(), 29, 10));
    assert!(text.contains("BLE"), "{text}");
}

#[test]
fn layouts_fit_both_languages_at_supported_small_viewports() {
    for lang in [Lang::En, Lang::Zh] {
        let mut app = conversation();
        app.lang = lang;
        for width in [4, 16, 30, 48, 80] {
            for height in [0, 1, 2, 3, 4, 5, 12, 20] {
                let rows = assistant::full_lines(&app, &app.theme(), width, height);
                assert!(rows.len() <= height as usize);
                for row in rows {
                    assert!(row.width() <= width as usize, "{width}x{height}: {row:?}");
                }
            }
        }
    }
}

#[test]
fn short_history_windows_keep_the_selected_thread_visible_and_resume_without_data_loss() {
    let mut app = App {
        screen: Rect::new(0, 0, 80, 16),
        ..App::default()
    };
    app.assistant.chats = (0..20)
        .map(|i| assistant::ChatState {
            id: i,
            title: format!("saved-thread-{i:02}"),
            ..assistant::ChatState::default()
        })
        .collect();
    app.assistant.active = 19;
    linkr_tui::dialogs::open_chat_pick(&mut app);
    assert!(ui::render_to_text(&app, 80, 16).contains("saved-thread-19"));
    for _ in 0..19 {
        linkr_tui::dialogs::handle_key(&mut app, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
    }
    assert!(ui::render_to_text(&app, 80, 16).contains("saved-thread-00"));
    linkr_tui::dialogs::handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(app.assistant.active, 0);
    assert_eq!(app.assistant.chats.len(), 20);
    assert!(app.dialog.is_none());
}

#[test]
fn saving_with_an_invalid_hidden_value_reveals_the_field_that_needs_attention() {
    let mut app = App {
        screen: Rect::new(0, 0, 80, 16),
        ..App::default()
    };
    app.assistant.endpoint = "http://localhost/v1".into();
    app.assistant.model = "model".into();
    app.assistant.context_window = "32768".into();
    app.assistant.max_tokens = "bad".into();
    assistant::open_settings(&mut app, assistant::AgentField::Endpoint);
    linkr_tui::dialogs::handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL),
    );
    assert!(app.assistant.settings_advanced);
    assert_eq!(app.assistant.focus, assistant::AgentField::MaxTokens);
    let frame = ui::render_to_text(&app, 80, 16);
    assert!(
        frame.contains(assistant::AgentField::MaxTokens.label(app.lang)),
        "{frame}"
    );
    assert!(!app.assistant.configured);
}

#[test]
fn the_uart_click_target_tracks_the_visible_baud_field_at_each_supported_width() {
    for width in [40, 60, 80, 120, 160] {
        for lang in [linkr_tui::i18n::Lang::Zh, linkr_tui::i18n::Lang::En] {
            let mut app = App {
                lang,
                screen: Rect::new(0, 0, width, 24),
                ..App::default()
            };
            for reported in [None, Some(9600)] {
                app.baud = reported;
                let text = ui::render_to_text(&app, width, 24);
                let top = text.lines().next().unwrap();
                let label = ui::chrome::baud_label(&app);
                let at = top.find(&label).expect("the baud field stays visible");
                let column = linkr_tui::text::columns(&top[..at]) as u16;
                let area = ui::baud_button_rect(&app, app.screen).unwrap();
                assert_eq!(area.x, column, "{lang:?} at {width}");
                assert_eq!(area.width as usize, linkr_tui::text::columns(&label));
            }
        }
    }
}
