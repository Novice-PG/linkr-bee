//! Every user-visible string, in both languages, in one table.
//!
//! Views never hold a literal: they call [`Lang::strings`] and read a field.
//! The two tables are `const`, so a missing translation is a compile error
//! rather than a blank row at runtime — the failure mode the old string
//! scattering produced.

use crate::event::{ConnectionState, View};

/// The interface language.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Lang {
    #[default]
    Zh,
    En,
}

/// One language's whole vocabulary.
#[derive(Debug, Clone, Copy)]
pub struct Strings {
    // --- technical tokens -------------------------------------------------
    // Deliberately untranslated: the design writes `RX`, `TX`, `Auto` and
    // `dBm` in the Chinese interface too, and keeping them Latin is what keeps
    // the top bar the same width whichever language is in force.
    /// Prefix of the receive counter (`RX`).
    pub rx: &'static str,
    /// Prefix of the transmit counter (`TX`).
    pub tx: &'static str,
    /// The auto-scroll indicator.
    pub autoscroll: &'static str,
    /// The unit of the signal reading beside the state (`dBm`).
    pub signal_unit: &'static str,
    /// What a slot shows before the device has reported a value.
    ///
    /// One glyph for every such slot, as the design has it: the rule spans its
    /// cell, so an unknown speed and an unknown signal read as the same
    /// absence rather than as two different kinds of dash.
    pub value_unknown: &'static str,
    /// The colon and its spacing after a field label.
    ///
    /// Chinese uses the full-width colon, which carries its own space; English
    /// needs an explicit one. Keeping both in the table is what lets the two
    /// rows be laid out by the same code.
    pub label_separator: &'static str,

    // --- sidebar ---------------------------------------------------------
    /// Section header for the connection block.
    pub section_connection: &'static str,
    /// Section header for the view list.
    pub section_views: &'static str,
    /// Section header for the quick-send list.
    pub section_quick_send: &'static str,
    /// Section header for the watch findings.
    pub section_watch: &'static str,
    /// `Transport: <name>` prefix.
    pub transport_label: &'static str,
    /// `Device: <name>` prefix.
    pub device_label: &'static str,
    /// The connect action.
    pub connect: &'static str,
    /// The switch-device action.
    pub switch_device: &'static str,
    /// Suffix on the transport row when a CLI flag pinned it.
    pub transport_locked: &'static str,
    /// Hint under the quick-send header.
    pub quick_send_hint: &'static str,
    /// Shown in the watch section when nothing has been found.
    pub watch_empty: &'static str,

    // --- bottom bar ------------------------------------------------------
    /// The focus tag's word for the terminal pane.
    pub focus_terminal: &'static str,
    /// The focus tag's word for the sidebar.
    pub focus_sidebar: &'static str,
    /// The palette hint.
    pub hint_palette: &'static str,
    /// The help hint.
    pub hint_help: &'static str,
    /// The view-rota hint.
    pub hint_views: &'static str,
    /// The quit hint.
    pub hint_quit: &'static str,

    // --- views -----------------------------------------------------------
    /// Title of the serial terminal view.
    pub view_terminal: &'static str,
    /// Title of the diagnostics view.
    pub view_diagnostics: &'static str,
    /// Title of the network view.
    pub view_network: &'static str,
    /// Title of the assistant view.
    pub view_assistant: &'static str,
    /// Title of the transfer view.
    pub view_transfer: &'static str,
    /// Placeholder in a view whose body has no content yet.
    pub view_empty: &'static str,

    // --- diagnostics view -------------------------------------------------
    /// Prefix of the "last read" stamp (`@i? · updated 19:08:06`).
    pub diag_updated: &'static str,
    /// Shown before diagnostics have ever been read.
    pub diag_not_read: &'static str,
    /// Why there is nothing to read: management commands are BLE-only.
    pub diag_no_ble: &'static str,
    /// Shown while `@i?` is in flight.
    pub diag_loading: &'static str,
    /// Shown when a read completed and reported nothing.
    pub diag_empty: &'static str,
    /// The keys this view answers to.
    pub diag_keys_hint: &'static str,
    /// The local failure when the link closed before a reply arrived.
    pub diag_poll_closed: &'static str,
    /// Grid label for the firmware version.
    pub diag_label_firmware: &'static str,
    /// Grid label for the device's uptime.
    pub diag_label_uptime: &'static str,
    /// Grid label for who may use the BLE link.
    pub diag_label_ble_access: &'static str,
    /// Grid label for the UART ring buffer.
    pub diag_label_uart_buffer: &'static str,
    /// Grid label for the upload queue.
    pub diag_label_upload_queue: &'static str,
    /// `sys.owner` is `0`: the link is open to anyone.
    pub diag_access_open: &'static str,
    /// `sys.owner` is someone else: access is scoped to one host.
    pub diag_access_scoped: &'static str,
    /// Prefix of the link security level (`link L2`).
    pub diag_link_level: &'static str,

    // --- notices ---------------------------------------------------------
    /// "Theme: light" and friends — the toast shown by the toggle.
    pub theme_switched_prefix: &'static str,
    /// The word for the dark theme in a notice.
    pub theme_dark: &'static str,
    /// The word for the light theme in a notice.
    pub theme_light: &'static str,
    /// The word for a finding awaiting review.
    pub pending_review: &'static str,
}

const ZH: Strings = Strings {
    rx: "RX",
    tx: "TX",
    autoscroll: "Auto",
    signal_unit: "dBm",
    value_unknown: "─",
    label_separator: "：",
    section_connection: "连接",
    section_views: "视图",
    section_quick_send: "快捷发送",
    section_watch: "监控",
    transport_label: "传输方式",
    device_label: "设备",
    connect: "连接",
    switch_device: "切换设备",
    transport_locked: "（已锁定）",
    quick_send_hint: "Enter 发送 · reboot 先确认",
    watch_empty: "暂无发现",
    focus_terminal: "终端",
    focus_sidebar: "侧栏",
    hint_palette: "Ctrl+P 面板",
    hint_help: "F1 帮助",
    hint_views: "F2-F6 视图",
    hint_quit: "Ctrl+Q 退出",
    view_terminal: "串口终端",
    view_diagnostics: "诊断",
    view_network: "网络",
    view_assistant: "助手",
    view_transfer: "文件传输",
    view_empty: "此视图尚未实现",
    diag_updated: "@i? · 已更新",
    diag_not_read: "@i? 尚未读取",
    diag_no_ble: "通过 BLE 连接后才能读取诊断（管理命令仅限 BLE）。",
    diag_loading: "正在读取 @i?…",
    diag_empty: "暂无设备信息。",
    diag_keys_hint: "r 刷新 · PgUp/PgDn 滚动 · F2 终端",
    diag_poll_closed: "诊断数据到达前连接已断开",
    diag_label_firmware: "固件",
    diag_label_uptime: "运行时间",
    diag_label_ble_access: "BLE 访问",
    diag_label_uart_buffer: "UART 缓冲",
    diag_label_upload_queue: "上传队列",
    diag_access_open: "开放",
    diag_access_scoped: "受限",
    diag_link_level: "链路 L",
    theme_switched_prefix: "主题",
    theme_dark: "深色",
    theme_light: "浅色",
    pending_review: "待确认",
};

const EN: Strings = Strings {
    rx: "RX",
    tx: "TX",
    autoscroll: "Auto",
    signal_unit: "dBm",
    value_unknown: "─",
    label_separator: ": ",
    section_connection: "Connection",
    section_views: "Views",
    section_quick_send: "Quick send",
    section_watch: "Watch",
    transport_label: "Transport",
    device_label: "Device",
    connect: "Connect",
    switch_device: "Switch device",
    transport_locked: "(locked)",
    // Fits the 29-column sidebar with three columns to spare. The longer
    // `Enter sends · reboot asks first` overflowed the column and wrapped onto a
    // second row, which pushed every section below it one row down — the
    // sidebar then sat differently in English than in Chinese.
    quick_send_hint: "Enter sends · reboot asks",
    watch_empty: "no findings yet",
    focus_terminal: "Terminal",
    focus_sidebar: "Sidebar",
    hint_palette: "Ctrl+P palette",
    hint_help: "F1 help",
    hint_views: "F2-F6 views",
    hint_quit: "Ctrl+Q quit",
    view_terminal: "Serial Terminal",
    view_diagnostics: "Diagnostics",
    view_network: "Network",
    view_assistant: "Assistant",
    view_transfer: "File transfer",
    view_empty: "This view is not implemented yet",
    diag_updated: "@i? · updated",
    diag_not_read: "@i? not read yet",
    diag_no_ble: "Connect over BLE to read diagnostics (management commands are BLE-only).",
    diag_loading: "reading @i?…",
    diag_empty: "No device information yet.",
    diag_keys_hint: "r refresh · PgUp/PgDn scroll · F2 terminal",
    diag_poll_closed: "disconnected before diagnostics arrived",
    diag_label_firmware: "Firmware",
    diag_label_uptime: "Uptime",
    diag_label_ble_access: "BLE access",
    diag_label_uart_buffer: "UART Buffer",
    diag_label_upload_queue: "Upload Queue",
    diag_access_open: "open",
    diag_access_scoped: "scoped",
    diag_link_level: "link L",
    theme_switched_prefix: "Theme",
    theme_dark: "dark",
    theme_light: "light",
    pending_review: "asks first",
};

impl Lang {
    /// This language's table.
    pub fn strings(self) -> &'static Strings {
        match self {
            Lang::Zh => &ZH,
            Lang::En => &EN,
        }
    }

    /// The name of a view in this language.
    pub fn view_name(self, view: View) -> &'static str {
        let s = self.strings();
        match view {
            View::Terminal => s.view_terminal,
            View::Diagnostics => s.view_diagnostics,
            View::Network => s.view_network,
            View::Assistant => s.view_assistant,
            View::Transfer => s.view_transfer,
        }
    }

    /// The connection state as it reads in the top bar and the toast.
    pub fn state_name(self, state: ConnectionState) -> &'static str {
        match (self, state) {
            (Lang::Zh, ConnectionState::Disconnected) => "未连接",
            (Lang::Zh, ConnectionState::Connecting) => "连接中…",
            (Lang::Zh, ConnectionState::Connected) => "已连接",
            (Lang::Zh, ConnectionState::Failed) => "连接失败",
            (Lang::En, ConnectionState::Disconnected) => "disconnected",
            (Lang::En, ConnectionState::Connecting) => "connecting…",
            (Lang::En, ConnectionState::Connected) => "connected",
            (Lang::En, ConnectionState::Failed) => "connection failed",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both tables exist and every field is filled: an empty string is the
    /// blank-row failure this table exists to prevent.
    #[test]
    fn both_languages_are_complete() {
        for lang in [Lang::Zh, Lang::En] {
            let s = lang.strings();
            let all = [
                s.rx,
                s.tx,
                s.autoscroll,
                s.signal_unit,
                s.value_unknown,
                s.label_separator,
                s.section_connection,
                s.section_views,
                s.section_quick_send,
                s.section_watch,
                s.transport_label,
                s.device_label,
                s.connect,
                s.switch_device,
                s.transport_locked,
                s.quick_send_hint,
                s.watch_empty,
                s.focus_terminal,
                s.focus_sidebar,
                s.hint_palette,
                s.hint_help,
                s.hint_views,
                s.hint_quit,
                s.view_terminal,
                s.view_diagnostics,
                s.view_network,
                s.view_assistant,
                s.view_transfer,
                s.view_empty,
                s.diag_updated,
                s.diag_not_read,
                s.diag_no_ble,
                s.diag_loading,
                s.diag_empty,
                s.diag_keys_hint,
                s.diag_poll_closed,
                s.diag_label_firmware,
                s.diag_label_uptime,
                s.diag_label_ble_access,
                s.diag_label_uart_buffer,
                s.diag_label_upload_queue,
                s.diag_access_open,
                s.diag_access_scoped,
                s.diag_link_level,
                s.theme_switched_prefix,
                s.theme_dark,
                s.theme_light,
                s.pending_review,
            ];
            for field in all {
                assert!(!field.trim().is_empty(), "empty string in {lang:?}");
            }
        }
    }

    /// The four connection states are all named, in both languages, and no two
    /// share a label — otherwise the dot and the text could disagree.
    #[test]
    fn every_connection_state_has_its_own_label() {
        for lang in [Lang::Zh, Lang::En] {
            let labels: Vec<&str> = [
                ConnectionState::Disconnected,
                ConnectionState::Connecting,
                ConnectionState::Connected,
                ConnectionState::Failed,
            ]
            .into_iter()
            .map(|state| lang.state_name(state))
            .collect();
            for label in &labels {
                assert!(!label.trim().is_empty());
            }
            let mut unique = labels.clone();
            unique.sort_unstable();
            unique.dedup();
            assert_eq!(
                unique.len(),
                labels.len(),
                "duplicate state label in {lang:?}"
            );
        }
    }

    /// Every diagnostics entry is genuinely translated: a copy that forgot to
    /// translate would otherwise pass as the Chinese one. The one label that is
    /// deliberately untranslated (`WiFi`) is not in this table at all.
    #[test]
    fn the_diagnostics_table_is_translated() {
        let pairs = |lang: Lang| -> Vec<(&'static str, &'static str)> {
            let s = lang.strings();
            vec![
                ("diag_updated", s.diag_updated),
                ("diag_not_read", s.diag_not_read),
                ("diag_no_ble", s.diag_no_ble),
                ("diag_loading", s.diag_loading),
                ("diag_empty", s.diag_empty),
                ("diag_keys_hint", s.diag_keys_hint),
                ("diag_poll_closed", s.diag_poll_closed),
                ("diag_label_firmware", s.diag_label_firmware),
                ("diag_label_uptime", s.diag_label_uptime),
                ("diag_label_ble_access", s.diag_label_ble_access),
                ("diag_label_uart_buffer", s.diag_label_uart_buffer),
                ("diag_label_upload_queue", s.diag_label_upload_queue),
                ("diag_access_open", s.diag_access_open),
                ("diag_access_scoped", s.diag_access_scoped),
                ("diag_link_level", s.diag_link_level),
            ]
        };
        let zh = pairs(Lang::Zh);
        let en = pairs(Lang::En);
        assert_eq!(zh.len(), 15, "the diagnostics view carries 15 entries");
        for ((name, zh), (_, en)) in zh.iter().zip(&en) {
            assert_ne!(zh, en, "{name} is untranslated");
        }
    }

    /// The technical tokens are the same in both languages, and the value
    /// placeholder is one column wide. Both matter for the same reason: the
    /// chrome is laid out to the column, and a token that changed width with the
    /// language — or a placeholder that rendered as two columns — would move
    /// everything after it.
    #[test]
    fn the_technical_tokens_do_not_move_with_the_language() {
        let zh = Lang::Zh.strings();
        let en = Lang::En.strings();
        for (name, zh, en) in [
            ("rx", zh.rx, en.rx),
            ("tx", zh.tx, en.tx),
            ("autoscroll", zh.autoscroll, en.autoscroll),
            ("signal_unit", zh.signal_unit, en.signal_unit),
            ("value_unknown", zh.value_unknown, en.value_unknown),
        ] {
            assert_eq!(zh, en, "{name} differs between the languages");
        }
        for lang in [Lang::Zh, Lang::En] {
            let placeholder = lang.strings().value_unknown;
            assert_eq!(
                crate::text::columns(placeholder),
                1,
                "{lang:?} placeholder {placeholder:?} is not one column wide"
            );
        }
    }

    /// The label separator is genuinely localised, and it costs the same number
    /// of columns in both languages: the full-width Chinese colon carries its
    /// own gap, so the English one has to spell that gap out. A separator that
    /// cost more in one language would move every value in that language.
    #[test]
    fn the_label_separator_keeps_the_value_in_one_column() {
        for lang in [Lang::Zh, Lang::En] {
            let s = lang.strings();
            let row = format!("{}{}{}", s.device_label, s.label_separator, "X");
            assert!(row.ends_with('X'), "{row:?}");
            assert_eq!(
                crate::text::columns(s.label_separator),
                2,
                "{lang:?}: the separator is not a colon and a gap"
            );
            assert!(
                !s.label_separator.starts_with(' '),
                "{lang:?}: the separator starts with the gap"
            );
        }
        // The Chinese colon is a colon and the gap is inside it; the English one
        // is a colon plus an explicit space.
        let zh = Lang::Zh.strings().label_separator;
        let en = Lang::En.strings().label_separator;
        assert_ne!(zh, en);
        assert_eq!(zh, "：");
        assert_eq!(en, ": ");
    }

    /// Every view has a title in both languages, and the titles differ between
    /// them (a missing translation would otherwise pass as the Chinese one).
    #[test]
    fn every_view_is_named_in_both_languages() {
        for view in View::ALL {
            for lang in [Lang::Zh, Lang::En] {
                assert!(!lang.view_name(view).trim().is_empty());
            }
            assert_ne!(
                Lang::Zh.view_name(view),
                Lang::En.view_name(view),
                "{view:?} is untranslated"
            );
        }
    }
}
