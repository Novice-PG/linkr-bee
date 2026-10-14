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

    // --- network view -----------------------------------------------------
    /// The WiFi section heading.
    pub net_wifi_section: &'static str,
    /// Why WiFi management needs BLE.
    pub net_wifi_needs_ble: &'static str,
    /// The network-name (SSID) field label.
    pub net_ssid_label: &'static str,
    /// The password field label.
    pub net_password_label: &'static str,
    /// The value an empty field shows.
    pub net_empty_value: &'static str,
    /// Suffix marking an action as BLE-only (`（仅限 BLE）`).
    pub net_ble_only: &'static str,
    /// The scan action.
    pub net_scan: &'static str,
    /// The connect action.
    pub net_connect: &'static str,
    /// The WiFi-off action.
    pub net_wifi_off: &'static str,
    /// The status action.
    pub net_status: &'static str,
    /// The scan-results section heading.
    pub net_results_section: &'static str,
    /// Shown when a scan has produced nothing.
    pub net_no_results: &'static str,
    /// The WebDAV section heading.
    pub net_webdav_section: &'static str,
    /// The WebDAV target URL label.
    pub net_url_label: &'static str,
    /// The set action.
    pub net_set: &'static str,
    /// The disable action.
    pub net_disable: &'static str,
    /// The WebDAV state prefix (`on`/`off`).
    pub net_state_on: &'static str,
    pub net_state_off: &'static str,

    // --- transfer view ----------------------------------------------------
    /// The outgoing-files section heading.
    pub xfer_out_section: &'static str,
    /// The incoming-files section heading.
    pub xfer_in_section: &'static str,
    /// Shown when no transfer has happened yet.
    pub xfer_empty: &'static str,
    /// The upload command hint.
    pub xfer_upload_hint: &'static str,
    /// The download command hint.
    pub xfer_download_hint: &'static str,
    /// A one-line transfer record: name, size, state.
    pub xfer_record_failed: &'static str,
    pub xfer_record_saved: &'static str,
    pub xfer_record_unverified: &'static str,
    pub xfer_record_pending: &'static str,
    /// How a ZMODEM transfer is started — the palette route, which is the one
    /// the keyboard really offers.
    pub xfer_zmodem_hint: &'static str,
    /// Prefix of the banner and the notice for a send (`sending to the
    /// device: <file>`).
    pub xfer_started_out: &'static str,
    /// Prefix of the banner and the notice for a receive (`receiving into:
    /// <directory>`).
    pub xfer_started_in: &'static str,
    /// Prefix of the notice raised when a transfer ends cleanly.
    pub xfer_finished: &'static str,
    /// Prefix of the notice raised when a transfer is given up on.
    pub xfer_cancelled: &'static str,
    /// Shown when a transfer was asked for while one is already running.
    pub xfer_busy: &'static str,
    /// Shown when a transfer was asked for with no link.
    pub xfer_needs_link: &'static str,
    /// Prefix of the notice raised when a transfer cannot be started at all.
    pub xfer_start_failed: &'static str,
    /// Prefix of the notice raised for each file that arrives.
    pub xfer_saved_notice: &'static str,
    /// Prefix of the notice raised for each file that does not.
    pub xfer_failed_notice: &'static str,
    /// Shown when a send was asked for with no file named.
    pub xfer_no_path: &'static str,

    // --- assistant view ---------------------------------------------------
    /// The session section heading.
    pub asst_session_section: &'static str,
    /// The tasks section heading.
    pub asst_tasks_section: &'static str,
    /// Shown when no task has been recorded for this device.
    pub asst_no_tasks: &'static str,
    /// Shown when the agent is not configured.
    pub asst_not_configured: &'static str,
    /// The notes section heading.
    pub asst_notes_section: &'static str,
    /// Shown when the device has no durable notes.
    pub asst_no_notes: &'static str,
    /// The execution-mode row label.
    pub asst_mode_label: &'static str,
    /// The three mode names.
    pub asst_mode_auto: &'static str,
    pub asst_mode_manual: &'static str,
    pub asst_mode_full_auto: &'static str,
    /// The token-usage row label.
    pub asst_usage_label: &'static str,
    /// The pending-approval row.
    pub asst_awaiting: &'static str,

    // --- notices ---------------------------------------------------------
    /// "Theme: light" and friends — the toast shown by the toggle.
    pub theme_switched_prefix: &'static str,
    /// The word for the dark theme in a notice.
    pub theme_dark: &'static str,
    /// The word for the light theme in a notice.
    pub theme_light: &'static str,
    /// The word for a finding awaiting review.
    pub pending_review: &'static str,

    // --- the link ---------------------------------------------------------
    /// Prefix of the notice raised when a link comes up.
    pub link_opened: &'static str,
    /// Prefix of the notice raised when a link ends.
    pub link_closed: &'static str,
    /// Prefix of the notice raised when a dial fails.
    pub link_failed: &'static str,
    /// Shown when something was to be sent and there was no link to send it
    /// over. A silent drop reads as success, which for `reboot` is the worst
    /// possible lie.
    pub not_connected: &'static str,

    // --- the switches ------------------------------------------------------
    /// Prefix of the notice raised when the interface language changes.
    pub language_switched_prefix: &'static str,
    /// The word for Chinese in that notice. Written in its own language, the
    /// way a language picker names a language.
    pub language_zh: &'static str,
    /// The word for English in that notice.
    pub language_en: &'static str,
    /// Shown when the transport cannot be switched because the command line
    /// pinned it.
    pub transport_is_locked: &'static str,
    /// Shown when there is nothing to dial — no bridge, no port.
    pub no_link: &'static str,
    /// Shown when `switch device` has nothing to switch to: one device is
    /// known, so the action is a no-op that says so rather than doing nothing.
    pub no_other_device: &'static str,
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
    net_wifi_section: "WiFi",
    net_wifi_needs_ble: "通过 BLE 连接后才能管理 WiFi（局域网传输方式没有管理通道）。",
    net_ssid_label: "网络名称",
    net_password_label: "密码",
    net_empty_value: "—",
    net_ble_only: "（仅限 BLE）",
    net_scan: "[扫描]",
    net_connect: "[连接 WiFi]",
    net_wifi_off: "[WiFi 关闭]",
    net_status: "[WiFi 状态]",
    net_results_section: "扫描结果",
    net_no_results: "暂无结果",
    net_webdav_section: "WebDAV 日志上传",
    net_url_label: "目标 URL",
    net_set: "[设置]",
    net_disable: "[关闭]",
    net_state_on: "开启",
    net_state_off: "关闭",
    xfer_out_section: "发送到设备",
    xfer_in_section: "从设备取回",
    xfer_empty: "暂无传输记录。",
    xfer_upload_hint: "上传：put <本地文件> <目标路径>",
    xfer_download_hint: "下载：get <目标路径> <本地文件>",
    xfer_record_failed: "失败",
    xfer_record_saved: "已保存",
    xfer_record_unverified: "未校验",
    xfer_record_pending: "进行中",
    xfer_zmodem_hint: "ZMODEM：Ctrl+P 面板 → 发送文件… / 接收文件…",
    xfer_started_out: "正在发送到设备：",
    xfer_started_in: "正在接收，写入：",
    xfer_finished: "传输结束。",
    xfer_cancelled: "传输已取消：",
    xfer_busy: "已有传输在进行中。",
    xfer_needs_link: "未连接——没有开始传输。",
    xfer_start_failed: "无法开始传输：",
    xfer_saved_notice: "已保存：",
    xfer_failed_notice: "传输失败：",
    xfer_no_path: "请输入要发送的文件路径。",
    asst_session_section: "会话",
    asst_tasks_section: "任务",
    asst_no_tasks: "此设备暂无任务记录。",
    asst_not_configured: "未配置模型端点（Ctrl+P 打开面板设置）。",
    asst_notes_section: "设备笔记",
    asst_no_notes: "暂无笔记。",
    asst_mode_label: "执行模式",
    asst_mode_auto: "自动（仅白名单命令免确认）",
    asst_mode_manual: "手动（每次都确认）",
    asst_mode_full_auto: "全自动（危险命令仍确认）",
    asst_usage_label: "Token 用量",
    asst_awaiting: "等待确认",
    link_opened: "链路已建立：",
    link_closed: "链路已断开：",
    link_failed: "连接失败：",
    not_connected: "未连接——没有发送任何内容。",
    language_switched_prefix: "界面语言",
    language_zh: "中文",
    language_en: "English",
    transport_is_locked: "传输方式已被命令行锁定，无法切换。",
    no_link: "没有配置链路——无目标可连接。",
    no_other_device: "没有其他设备可切换。",
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
    net_wifi_section: "WiFi",
    net_wifi_needs_ble:
        "Connect over BLE to manage WiFi (the LAN transport has no management channel).",
    net_ssid_label: "Network name",
    net_password_label: "Password",
    net_empty_value: "—",
    net_ble_only: " (BLE only)",
    net_scan: "[Scan]",
    net_connect: "[Connect WiFi]",
    net_wifi_off: "[WiFi off]",
    net_status: "[WiFi status]",
    net_results_section: "Scan results",
    net_no_results: "no results yet",
    net_webdav_section: "WebDAV log upload",
    net_url_label: "Target URL",
    net_set: "[Set]",
    net_disable: "[Disable]",
    net_state_on: "on",
    net_state_off: "off",
    xfer_out_section: "To the device",
    xfer_in_section: "From the device",
    xfer_empty: "No transfers yet.",
    xfer_upload_hint: "Upload: put <local file> <target path>",
    xfer_download_hint: "Download: get <target path> <local file>",
    xfer_record_failed: "failed",
    xfer_record_saved: "saved",
    xfer_record_unverified: "unverified",
    xfer_record_pending: "running",
    xfer_zmodem_hint: "ZMODEM: Ctrl+P palette → Send a file… / Receive files…",
    xfer_started_out: "sending to the device: ",
    xfer_started_in: "receiving into: ",
    xfer_finished: "Transfer finished.",
    xfer_cancelled: "Transfer cancelled: ",
    xfer_busy: "A transfer is already running.",
    xfer_needs_link: "Not connected — no transfer was started.",
    xfer_start_failed: "cannot start the transfer: ",
    xfer_saved_notice: "saved: ",
    xfer_failed_notice: "transfer failed: ",
    xfer_no_path: "Name a file to send.",
    asst_session_section: "Session",
    asst_tasks_section: "Tasks",
    asst_no_tasks: "No tasks recorded for this device.",
    asst_not_configured: "No model endpoint configured (Ctrl+P opens the palette).",
    asst_notes_section: "Device notes",
    asst_no_notes: "no notes",
    asst_mode_label: "Execution mode",
    asst_mode_auto: "auto (only allowlisted commands run unattended)",
    asst_mode_manual: "manual (every send is confirmed)",
    asst_mode_full_auto: "full auto (destructive commands still ask)",
    asst_usage_label: "Token usage",
    asst_awaiting: "awaiting approval",
    link_opened: "link open: ",
    link_closed: "link closed: ",
    link_failed: "connect failed: ",
    not_connected: "Not connected — nothing was sent.",
    language_switched_prefix: "Interface language",
    language_zh: "中文",
    language_en: "English",
    transport_is_locked: "The transport is pinned by the command line.",
    no_link: "No link configured — nothing to dial.",
    no_other_device: "No other device to switch to yet.",
};

/// A bilingual interface string: `[English, Chinese]`.
///
/// The flat [`Strings`] table is for the vocabulary the whole interface shares —
/// labels, placeholders, the words in the bars. Text that belongs to exactly one
/// module (every palette action, every dialog) is kept *with* that module as a
/// list of these, so a module's words are readable next to the code that paints
/// them, and the two halves can never be edited apart.
pub type Entry = [&'static str; 2];

/// The text of an entry in `lang`.
pub const fn entry(text: Entry, lang: Lang) -> &'static str {
    match lang {
        Lang::En => text[0],
        Lang::Zh => text[1],
    }
}

/// Assert a table is complete: both halves carry text, and they differ — a
/// copied English string is a missing translation, not a translation.
///
/// The `pub` is for the sibling modules' tests; the assert is the same guard
/// the flat table has, applied to the per-module tables.
pub fn assert_bilingual(entries: &[Entry]) {
    for [en, zh] in entries {
        assert!(!en.trim().is_empty(), "an english string is empty");
        assert!(!zh.trim().is_empty(), "a chinese string is empty");
        assert_ne!(en, zh, "{en:?} was never translated");
    }
}

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
                s.net_wifi_section,
                s.net_wifi_needs_ble,
                s.net_ssid_label,
                s.net_password_label,
                s.net_empty_value,
                s.net_ble_only,
                s.net_scan,
                s.net_connect,
                s.net_wifi_off,
                s.net_status,
                s.net_results_section,
                s.net_no_results,
                s.net_webdav_section,
                s.net_url_label,
                s.net_set,
                s.net_disable,
                s.net_state_on,
                s.net_state_off,
                s.xfer_out_section,
                s.xfer_in_section,
                s.xfer_empty,
                s.xfer_upload_hint,
                s.xfer_download_hint,
                s.xfer_record_failed,
                s.xfer_record_saved,
                s.xfer_record_unverified,
                s.xfer_record_pending,
                s.xfer_zmodem_hint,
                s.xfer_started_out,
                s.xfer_started_in,
                s.xfer_finished,
                s.xfer_cancelled,
                s.xfer_busy,
                s.xfer_needs_link,
                s.xfer_start_failed,
                s.xfer_saved_notice,
                s.xfer_failed_notice,
                s.xfer_no_path,
                s.asst_session_section,
                s.asst_tasks_section,
                s.asst_no_tasks,
                s.asst_not_configured,
                s.asst_notes_section,
                s.asst_no_notes,
                s.asst_mode_label,
                s.asst_mode_auto,
                s.asst_mode_manual,
                s.asst_mode_full_auto,
                s.asst_usage_label,
                s.asst_awaiting,
                s.link_opened,
                s.link_closed,
                s.link_failed,
                s.not_connected,
                s.language_switched_prefix,
                s.language_zh,
                s.language_en,
                s.transport_is_locked,
                s.no_link,
                s.no_other_device,
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
