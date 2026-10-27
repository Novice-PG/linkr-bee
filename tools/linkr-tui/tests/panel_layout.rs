use linkr_tui::app::{App, Focus};
use linkr_tui::event::View;
use linkr_tui::settings::SettingsStore;
use linkr_tui::ui;
use ratatui::layout::{Position, Rect};

fn app(width: u16, height: u16) -> App {
    App {
        screen: Rect::new(0, 0, width, height),
        ..App::default()
    }
}

#[test]
fn closing_each_side_gives_its_space_to_the_content() {
    let mut a = app(120, 30);
    let original = ui::regions_for(a.screen, a.sidebar_visible, a.panel_wanted());
    assert!(original.sidebar.width > 0 && original.assistant.width > 0);
    a.toggle_sidebar();
    let no_left = ui::regions_for(a.screen, a.sidebar_visible, a.panel_wanted());
    assert_eq!(no_left.sidebar.width, 0);
    assert!(no_left.centre.width > original.centre.width);
    a.toggle_assistant_panel();
    let content = ui::regions_for(a.screen, a.sidebar_visible, a.panel_wanted());
    assert_eq!(content.centre.width, 120);
    assert_eq!(content.assistant.width, 0);
}

#[test]
fn focusing_the_assistant_restores_layout_focus_and_draft() {
    let mut a = app(140, 30);
    a.focus = Focus::Assistant;
    a.assistant.chat_mut().composer = "保留输入 👩‍💻".into();
    a.toggle_focus_layout();
    assert_eq!(a.view, View::Assistant);
    assert!(!a.sidebar_visible && !a.assistant_panel);
    assert_eq!(ui::regions_for(a.screen, false, false).centre.width, 140);
    a.toggle_focus_layout();
    assert_eq!(a.view, View::Terminal);
    assert_eq!(a.focus, Focus::Assistant);
    assert!(a.sidebar_visible && a.assistant_panel);
    assert_eq!(a.assistant.chat().composer, "保留输入 👩‍💻");
}

#[test]
fn a_small_window_can_open_and_close_navigation_without_resizing_the_terminal() {
    let mut a = app(60, 20);
    a.toggle_sidebar();
    let drawer = ui::sidebar_drawer(&a, a.screen).expect("navigation opens as a drawer");
    assert!(drawer.width > 0);
    assert_eq!(a.focus, Focus::Sidebar);
    assert_eq!(
        ui::regions_for(a.screen, a.sidebar_visible, a.panel_wanted())
            .centre
            .width,
        60
    );
    a.toggle_sidebar();
    assert!(ui::sidebar_drawer(&a, a.screen).is_none());
    assert_eq!(a.focus, Focus::Terminal);
}

#[test]
fn a_small_window_opens_the_full_assistant_instead_of_an_invisible_panel() {
    let mut a = app(60, 20);
    a.assistant.chat_mut().composer = "draft".into();
    a.toggle_assistant_panel();
    assert_eq!(a.view, View::Assistant);
    assert!(ui::render_to_text(&a, 60, 20).contains("draft"));
    a.toggle_assistant_panel();
    assert_eq!(a.view, View::Terminal);
    assert!(!a.assistant_panel);
    assert_eq!(a.assistant.chat().composer, "draft");
}

#[test]
fn panel_choices_are_saved_but_temporary_focus_does_not_replace_them() {
    let path = std::env::temp_dir().join(format!("bee-layout-{}.json", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let mut a = app(140, 30);
    a.settings = Some(SettingsStore::at(&path));
    a.toggle_sidebar();
    let saved = SettingsStore::at(&path).read();
    assert_eq!(saved.sidebar_visible, Some(false));
    assert_eq!(saved.assistant_panel, Some(true));
    a.toggle_focus_layout();
    assert!(!a.assistant_panel);
    let focused = SettingsStore::at(&path).read();
    assert_eq!(focused.sidebar_visible, Some(false));
    assert_eq!(focused.assistant_panel, Some(true));
    assert_eq!(focused.view, Some(View::Terminal));
    a.toggle_focus_layout();
    a.toggle_assistant_panel();
    let saved = SettingsStore::at(&path).read();
    assert_eq!(saved.assistant_panel, Some(false));
    std::fs::remove_file(path).unwrap();
}

#[test]
fn every_drawn_layout_button_has_a_hit_target_in_both_languages_and_small_windows() {
    use linkr_tui::i18n::Lang;
    use linkr_tui::ui::layout_controls::{self, Control};
    for lang in [Lang::Zh, Lang::En] {
        for width in [40, 60, 80, 120, 160] {
            let mut a = app(width, 24);
            a.lang = lang;
            let painted = ui::render_to_text(&a, width, 24);
            let bottom = painted.lines().last().unwrap();
            for (key, action) in [
                ("F7", Control::Sidebar),
                ("F6", Control::Assistant),
                ("F8", Control::Focus),
            ] {
                let at = bottom.find(key).expect("the control must stay visible");
                let column = linkr_tui::text::columns(&bottom[..at]) as u16;
                assert_eq!(
                    layout_controls::hit(&a, a.screen, Position::new(column, 23)),
                    Some(action),
                    "{lang:?} at {width}"
                );
            }
        }
    }
}

#[test]
fn hiding_navigation_does_not_resurrect_ai_and_consume_the_freed_space() {
    let mut a = app(80, 24);
    assert_eq!(ui::regions_for(a.screen, true, true).assistant.width, 0);
    a.toggle_sidebar();
    let layout = ui::regions_for(a.screen, a.sidebar_visible, a.panel_wanted());
    assert_eq!(layout.centre.width, 80);
    assert_eq!(layout.assistant.width, 0);
}

#[test]
fn restoring_focus_after_shrinking_keeps_the_ai_draft_visible() {
    let mut a = app(140, 30);
    a.focus = Focus::Assistant;
    a.assistant.chat_mut().composer = "continue safely".into();
    a.toggle_focus_layout();
    a.screen = Rect::new(0, 0, 60, 20);
    a.toggle_focus_layout();
    assert_eq!(a.view, View::Assistant);
    assert_eq!(a.focus, Focus::Terminal);
    assert!(ui::render_to_text(&a, 60, 20).contains("continue safely"));
}
