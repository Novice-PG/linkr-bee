//! Network view: WiFi scan/connect form and WebDAV controls, all through
//! `@w`/`@d` management commands with the section 3.6 capability gates
//! (BLE-only; disabled over LAN).

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use tokio::sync::oneshot;
use unicode_width::UnicodeWidthStr;

use super::i18n::{strings, t, tr, Lang, MSG_SAVE_SETTINGS};
use super::replies::{
    parse_scan_line, parse_webdav_reply, parse_wifi_reply, webdav_state_text, wifi_state_text,
    ScanResult, WebdavStatus, WifiStatus,
};
use super::state::{App, Focus, TextField};
use crate::event::NoticeLevel;
use crate::protocol::mgmt::{MGMT_CAP_ASYNC_EVENTS, MGMT_CAP_WEBDAV, MGMT_CAP_WIFI};
use crate::protocol::MgmtReply;

strings! {
    NET_STATUS => "  connected network: {} · device IP: {}",
        "  当前网络：{} · 设备 IP：{}";
    NET_NO_BLE => "  Connect over BLE to manage WiFi (LAN transport has no management channel).",
        "  通过 BLE 连接后才能管理 WiFi（局域网传输方式没有管理通道）。";
    NET_FIELD_SSID => "Network name:", "网络名称：";
    NET_FIELD_PASSWORD => "Password:", "密码：";
    NET_FIELD_URL => "Target URL:", "目标 URL：";
    NET_ACT_SCAN => "Scan", "扫描";
    NET_ACT_CONNECT => "Connect WiFi", "连接 WiFi";
    NET_ACT_WIFI_OFF => "WiFi off", "WiFi 关闭";
    NET_ACT_WIFI_STATUS => "WiFi status", "WiFi 状态";
    NET_ACT_SET => "Set", "设置";
    NET_ACT_OFF => "Off", "关闭";
    NET_ACT_STATUS => "Status", "状态";
    NET_SUFFIX_BLE => " (BLE only)", "（仅限 BLE）";
    NET_SCAN_RESULTS => "Scan results", "扫描结果";
    NET_SCANNING => "  scanning…", "  扫描中…";
    NET_NO_RESULTS => "  no results yet", "  暂无结果";
    NET_WEBDAV_TITLE => "WebDAV log upload", "WebDAV 日志上传";
    NET_KEYS_HINT => "↑/↓ select · Enter act or edit · Esc to terminal",
        "↑/↓ 选择 · Enter 执行或编辑 · Esc 返回终端";
    NET_ERR_ENTER_SSID => "Enter a network name.", "请输入网络名称。";
    NET_ERR_SSID_LEN => "A network name is limited to {} characters.",
        "网络名称最多 {} 个字符。";
    NET_ERR_SSID_SHAPE => "A network name must not contain commas or control characters.",
        "网络名称不能包含逗号或控制字符。";
    NET_ERR_PASSWORD_LEN => "A password is limited to {} characters.", "密码最多 {} 个字符。";
    NET_ERR_PASSWORD_SHAPE => "A password must not contain control characters.",
        "密码不能包含控制字符。";
    NET_ERR_ENTER_URL => "Enter an http(s) URL first.", "请先输入 http(s) URL。";
    NET_ERR_URL_LEN => "A WebDAV URL is limited to {} characters.",
        "WebDAV URL 最多 {} 个字符。";
    NET_ERR_URL_PREFIX => "The WebDAV URL must start with http:// or https://.",
        "WebDAV URL 必须以 http:// 或 https:// 开头。";
    NET_ERR_URL_SHAPE => "The WebDAV URL must not contain whitespace or control characters.",
        "WebDAV URL 不能包含空白字符或控制字符。";
    NET_GATE_BLE => "Connect over BLE to manage WiFi and WebDAV.",
        "请通过 BLE 连接以管理 WiFi 和 WebDAV。";
    NET_FEEDBACK_IDLE => "Scan to select a nearby 2.4 GHz network.",
        "扫描并选择附近的 2.4 GHz 网络。";
    NET_FEEDBACK_SELECTED => "Selected {}.", "已选择 {}。";
    NET_FEEDBACK_SCANNING => "Scanning 2.4 GHz networks…", "正在扫描 2.4 GHz 网络…";
    NET_FEEDBACK_CONNECTING => "Connecting to {}…", "正在连接 {}…";
    NET_FEEDBACK_WIFI_OFF => "Disconnecting WiFi…", "正在断开 WiFi…";
    NET_FEEDBACK_FOUND => "Found {} networks.", "找到 {} 个网络。";
    NET_FEEDBACK_SCAN_FAILED => "Scan failed.", "扫描失败。";
    NET_FEEDBACK_REQUEST_FAILED => "Request failed.", "请求失败。";
    NET_FEEDBACK_REQUEST_CANCELLED => "Request cancelled.", "请求已取消。";
    NET_FEEDBACK_WIFI => "WiFi: {}", "WiFi：{}";
    NET_FEEDBACK_WIFI_IP => "WiFi: {} · {}", "WiFi：{} · {}";
    NET_FEEDBACK_WIFI_SENT => "WiFi request sent.", "WiFi 请求已发送。";
    NET_FEEDBACK_WEBDAV => "WebDAV: {}", "WebDAV：{}";
}

/// `ACCESSORY_LIMITS` of the web client.
pub const SSID_MAX: usize = 32;
pub const PASSWORD_MAX: usize = 64;
pub const WEBDAV_URL_MAX: usize = 256;
/// `WIFI_SCAN_TIMEOUT_MS` / `WIFI_OPERATION_TIMEOUT_SECS`.
const SCAN_WAIT: std::time::Duration = std::time::Duration::from_secs(35);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingKind {
    Scan,
    WifiConnect,
    WifiOff,
    WifiStatus,
    WebdavSet,
    WebdavOff,
    WebdavStatus,
}

/// One selectable row of the network view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetEntry {
    Ssid,
    Password,
    Webdav,
    Scan,
    WifiConnect,
    WifiOff,
    WifiStatus,
    WebdavSet,
    WebdavOff,
    WebdavStatus,
    Result(usize),
}

#[derive(Default)]
pub struct NetworkState {
    pub ssid: TextField,
    pub password: TextField,
    pub webdav: TextField,
    pub show_password: bool,
    pub selection: usize,
    pub scan: Vec<ScanResult>,
    pub scan_running: bool,
    /// Status line under the form (web `#wifiFeedback`), written in the
    /// language [`Self::lang`] carries — `sync_lang` keeps the two in step.
    pub feedback: String,
    /// `linkr-lang` this state writes its feedback strings in.
    pub lang: Lang,
    pub wifi_status: Option<WifiStatus>,
    pub webdav_status: Option<WebdavStatus>,
    pending: Vec<(PendingKind, oneshot::Receiver<Result<MgmtReply, String>>)>,
}

impl NetworkState {
    pub fn new() -> Self {
        Self {
            feedback: t(NET_FEEDBACK_IDLE, Lang::En).to_string(),
            ..Default::default()
        }
    }

    pub fn can_wifi(&self, app: &App) -> bool {
        let caps = app.capabilities();
        app.ble_connected() && caps & MGMT_CAP_WIFI != 0
    }

    pub fn can_wifi_scan(&self, app: &App) -> bool {
        self.can_wifi(app) && app.capabilities() & MGMT_CAP_ASYNC_EVENTS != 0
    }

    pub fn can_webdav(&self, app: &App) -> bool {
        app.ble_connected() && app.capabilities() & MGMT_CAP_WEBDAV != 0
    }
}

/// The selectable rows, **in the order `render_lines` draws them**: ↑/↓ walks
/// this list while the eye reads the screen, so any divergence makes the
/// cursor jump over rows that are visibly there. The scan results sit between
/// the WiFi block and the WebDAV block on screen, so they are selectable
/// between those two blocks as well.
pub fn entries(app: &App) -> Vec<NetEntry> {
    let mut list = vec![
        NetEntry::Ssid,
        NetEntry::Password,
        NetEntry::Scan,
        NetEntry::WifiConnect,
        NetEntry::WifiOff,
        NetEntry::WifiStatus,
    ];
    for i in 0..app.network.scan.len() {
        list.push(NetEntry::Result(i));
    }
    list.extend([
        NetEntry::Webdav,
        NetEntry::WebdavSet,
        NetEntry::WebdavOff,
        NetEntry::WebdavStatus,
    ]);
    list
}

/// Columns the row marker occupies, before the label.
const MARKER_COLUMNS: usize = 2;

/// One field row, and — when that row is the one being edited — where its
/// caret lands inside the line: the marker, `label `, then the caret's own
/// column in the text.
fn field_line(
    label: &str,
    field: &TextField,
    mask: bool,
    selected: bool,
) -> (Line<'static>, Option<usize>) {
    let (text, _) = field.display(if mask { Some('*') } else { None });
    let marker = if selected { "▸ " } else { "  " };
    let style = if selected {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Gray)
    };
    let value = if text.is_empty() {
        "–".to_string()
    } else {
        text
    };
    // An empty field draws a dash in the text's place, so a caret at column
    // zero sits *on* that dash rather than in front of nothing.
    let caret =
        selected.then(|| MARKER_COLUMNS + UnicodeWidthStr::width(label) + 1 + field.caret_column());
    (
        Line::from(vec![
            Span::styled(marker.to_string(), Style::default().fg(Color::Yellow)),
            Span::styled(format!("{label} "), Style::default().fg(Color::Cyan)),
            Span::styled(value, style),
        ]),
        caret,
    )
}

/// Push a field row and remember where its caret sits: the line index is the
/// row's own position, read before it is pushed so it cannot drift.
fn push_field(
    lines: &mut Vec<Line<'static>>,
    caret: &mut Option<(usize, usize)>,
    label: &str,
    field: &TextField,
    mask: bool,
    selected: bool,
) {
    let (line, column) = field_line(label, field, mask, selected);
    if let Some(column) = column {
        *caret = Some((lines.len(), column));
    }
    lines.push(line);
}

fn action_line(
    label: &str,
    enabled: bool,
    selected: bool,
    pending: bool,
    lang: Lang,
) -> Line<'static> {
    let marker = if selected { "▸ " } else { "  " };
    let mut style = if selected {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else if !enabled {
        Style::default().fg(Color::DarkGray)
    } else {
        Style::default().fg(Color::White)
    };
    if pending {
        style = style.fg(Color::LightBlue);
    }
    let suffix = if pending {
        " …"
    } else if !enabled {
        t(NET_SUFFIX_BLE, lang)
    } else {
        ""
    };
    Line::from(vec![
        Span::styled(marker.to_string(), Style::default().fg(Color::Yellow)),
        Span::styled(format!("[{label}]{suffix}"), style),
    ])
}

/// The state is built before the settings are read, so the language it writes
/// its feedback in is refreshed here (once per loop iteration, and on every
/// entry point that can write feedback). The standing idle line is re-rendered
/// in the new language; anything else the user triggered stays as it was.
fn sync_lang(app: &mut App) {
    let lang = app.lang();
    if app.network.lang == lang {
        return;
    }
    if app.network.feedback == t(NET_FEEDBACK_IDLE, app.network.lang) {
        app.network.feedback = t(NET_FEEDBACK_IDLE, lang).to_string();
    }
    app.network.lang = lang;
}

/// Body of the Network view (rendered in the center pane).
pub fn render_lines(app: &App) -> Vec<Line<'static>> {
    render_lines_at(app).0
}

/// The body, plus where the caret of the field being edited sits in it: the
/// index of that line, and the caret's column inside that line.
///
/// Reported rather than drawn — [`super::layout::center_caret`] turns it into
/// the terminal's cursor, because a caret cell of the field's own would push
/// a full-width row one column past the pane and wrap it onto the line below.
pub fn render_lines_at(app: &App) -> (Vec<Line<'static>>, Option<(usize, usize)>) {
    let lang = app.lang();
    let net = &app.network;
    // One row list for the whole frame: `is_sel` used to rebuild it for every
    // row it was asked about, i.e. (10 + n) allocations per row per frame.
    let items = entries(app);
    let selection = app.network.selection;
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut caret: Option<(usize, usize)> = None;

    lines.push(Line::from(Span::styled(
        "WiFi",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )));
    if let Some(status) = &net.wifi_status {
        let ssid = if status.ssid.is_empty() {
            "–"
        } else {
            &status.ssid
        };
        let ip = if status.ip.is_empty() {
            "–"
        } else {
            &status.ip
        };
        lines.push(Line::from(tr!(t(NET_STATUS, lang), ssid, ip)));
    }
    if !app.ble_connected() {
        lines.push(Line::from(Span::styled(
            t(NET_NO_BLE, lang),
            Style::default().fg(Color::DarkGray),
        )));
    }

    let wifi_ok = net.can_wifi(app);
    let scan_ok = net.can_wifi_scan(app);
    let pending = |kind: PendingKind| net.pending.iter().any(|(k, _)| *k == kind);

    push_field(
        &mut lines,
        &mut caret,
        t(NET_FIELD_SSID, lang),
        &net.ssid,
        false,
        is_sel(&items, selection, NetEntry::Ssid),
    );
    push_field(
        &mut lines,
        &mut caret,
        t(NET_FIELD_PASSWORD, lang),
        &net.password,
        !net.show_password,
        is_sel(&items, selection, NetEntry::Password),
    );
    lines.push(action_line(
        t(NET_ACT_SCAN, lang),
        scan_ok,
        is_sel(&items, selection, NetEntry::Scan),
        pending(PendingKind::Scan),
        lang,
    ));
    lines.push(action_line(
        t(NET_ACT_CONNECT, lang),
        wifi_ok,
        is_sel(&items, selection, NetEntry::WifiConnect),
        pending(PendingKind::WifiConnect) || pending(PendingKind::WifiOff),
        lang,
    ));
    lines.push(action_line(
        t(NET_ACT_WIFI_OFF, lang),
        wifi_ok,
        is_sel(&items, selection, NetEntry::WifiOff),
        pending(PendingKind::WifiOff),
        lang,
    ));
    lines.push(action_line(
        t(NET_ACT_WIFI_STATUS, lang),
        wifi_ok,
        is_sel(&items, selection, NetEntry::WifiStatus),
        pending(PendingKind::WifiStatus),
        lang,
    ));

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        t(NET_SCAN_RESULTS, lang),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )));
    if net.scan.is_empty() {
        lines.push(Line::from(Span::styled(
            if net.scan_running {
                t(NET_SCANNING, lang).to_string()
            } else {
                t(NET_NO_RESULTS, lang).to_string()
            },
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        for (i, result) in net.scan.iter().enumerate() {
            let selected_here = is_sel(&items, selection, NetEntry::Result(i));
            let marker = if selected_here { "▸ " } else { "  " };
            let rssi = match result.rssi {
                Some(v) => format!("{v} dBm"),
                None => "–".to_string(),
            };
            let channel = match result.channel {
                Some(v) => format!(" ch {v}"),
                None => String::new(),
            };
            let security = result
                .security
                .as_deref()
                .map(|s| format!(" {s}"))
                .unwrap_or_default();
            let style = if selected_here {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };
            lines.push(Line::from(vec![
                Span::styled(marker.to_string(), Style::default().fg(Color::Yellow)),
                Span::styled(result.ssid.clone(), style),
                Span::styled(
                    format!("  {rssi}{channel}{security}"),
                    Style::default().fg(Color::DarkGray),
                ),
            ]));
        }
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        t(NET_WEBDAV_TITLE, lang),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )));
    if let Some(status) = &net.webdav_status {
        let state = webdav_state_text(&status.state, lang);
        lines.push(Line::from(format!(
            "  {} · {}",
            state,
            if status.url.is_empty() {
                "–"
            } else {
                &status.url
            },
        )));
    }
    let dav_ok = net.can_webdav(app);
    push_field(
        &mut lines,
        &mut caret,
        t(NET_FIELD_URL, lang),
        &net.webdav,
        false,
        is_sel(&items, selection, NetEntry::Webdav),
    );
    lines.push(action_line(
        t(NET_ACT_SET, lang),
        dav_ok,
        is_sel(&items, selection, NetEntry::WebdavSet),
        pending(PendingKind::WebdavSet),
        lang,
    ));
    lines.push(action_line(
        t(NET_ACT_OFF, lang),
        dav_ok,
        is_sel(&items, selection, NetEntry::WebdavOff),
        pending(PendingKind::WebdavOff),
        lang,
    ));
    lines.push(action_line(
        t(NET_ACT_STATUS, lang),
        dav_ok,
        is_sel(&items, selection, NetEntry::WebdavStatus),
        pending(PendingKind::WebdavStatus),
        lang,
    ));

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        net.feedback.clone(),
        Style::default().fg(Color::LightYellow),
    )));
    lines.push(Line::from(Span::styled(
        t(NET_KEYS_HINT, lang),
        Style::default().fg(Color::DarkGray),
    )));
    (lines, caret)
}

fn is_sel(items: &[NetEntry], selection: usize, entry: NetEntry) -> bool {
    items.get(selection) == Some(&entry)
}

/// Screen line the selected row's `▸` is drawn on.
///
/// The frame is the only thing that knows where a row lands: the lines between
/// rows come and go with the two status blocks, so both directions below ask
/// the renderer rather than trying to re-derive the layout — the same lookup
/// `the_cursor_walks_the_rows_top_to_bottom` uses.
fn selection_line(app: &App) -> Option<usize> {
    render_lines(app)
        .iter()
        .position(|line| line.spans.first().map(|span| span.content.as_ref()) == Some("▸ "))
}

/// Bring the window to the selected row.
///
/// The network view is the one center pane that has both a scroll and a
/// cursor, and neither used to account for the other: ↑/↓/Tab/Home/End moved
/// the cursor through `entries()` while `center_scroll` only ever changed on
/// PageUp/PageDown, so a long scan list walked the `▸` off the bottom of the
/// pane and the next keypress moved a row nobody could see.
pub fn follow_selection(app: &mut App) {
    if let Some(line) = selection_line(app) {
        follow_line(app, line);
    }
}

/// Whether the selected row is on screen right now.
pub fn selection_visible(app: &App) -> bool {
    let Some(line) = selection_line(app) else {
        return false;
    };
    let top = app.center_scroll as usize;
    line >= top && line < top + app.center_height.max(1) as usize
}

/// Bring the selected row to the window that just moved.
///
/// The entries are drawn in [`entries`] order, so the row nearest the top of
/// the window names the selection. When the window has gone past every row —
/// only the feedback and the key hint live down there — there is nothing to
/// follow, and the window goes back to the cursor instead of leaving it
/// stranded off screen.
pub fn follow_scroll(app: &mut App) {
    if selection_visible(app) {
        return;
    }
    let rows = app.center_height.max(1) as usize;
    let top = app.center_scroll as usize;
    let len = entries(app).len();
    if len == 0 {
        return;
    }
    let saved = app.network.selection;
    let mut best: Option<usize> = None;
    let mut best_line = usize::MAX;
    for candidate in 0..len {
        app.network.selection = candidate;
        if let Some(line) = selection_line(app) {
            if line >= top && line < top + rows && line < best_line {
                best_line = line;
                best = Some(candidate);
            }
        }
    }
    match best {
        Some(found) => app.network.selection = found,
        None => {
            app.network.selection = saved;
            follow_selection(app);
        }
    }
}

/// `center_scroll` so `line` is one of the `rows` visible ones, moving the
/// window by as little as it takes.
fn follow_line(app: &mut App, line: usize) {
    let rows = app.center_height.max(1) as usize;
    let top = app.center_scroll as usize;
    let target = if line < top {
        line
    } else if line >= top + rows {
        line + 1 - rows
    } else {
        return;
    };
    app.center_scroll = u16::try_from(target).unwrap_or(u16::MAX);
}

// --- validation --------------------------------------------------------------

pub fn validate_ssid(ssid: &str, lang: Lang) -> Result<(), String> {
    if ssid.is_empty() {
        return Err(t(NET_ERR_ENTER_SSID, lang).to_string());
    }
    if ssid.chars().count() > SSID_MAX {
        return Err(tr!(t(NET_ERR_SSID_LEN, lang), SSID_MAX));
    }
    if ssid.contains(',') || ssid.chars().any(char::is_control) {
        return Err(t(NET_ERR_SSID_SHAPE, lang).to_string());
    }
    Ok(())
}

pub fn validate_password(password: &str, lang: Lang) -> Result<(), String> {
    if password.chars().count() > PASSWORD_MAX {
        return Err(tr!(t(NET_ERR_PASSWORD_LEN, lang), PASSWORD_MAX));
    }
    if password.chars().any(char::is_control) {
        return Err(t(NET_ERR_PASSWORD_SHAPE, lang).to_string());
    }
    Ok(())
}

pub fn validate_webdav_url(url: &str, lang: Lang) -> Result<(), String> {
    if url.is_empty() {
        return Err(t(NET_ERR_ENTER_URL, lang).to_string());
    }
    if url.chars().count() > WEBDAV_URL_MAX {
        return Err(tr!(t(NET_ERR_URL_LEN, lang), WEBDAV_URL_MAX));
    }
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(t(NET_ERR_URL_PREFIX, lang).to_string());
    }
    if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(t(NET_ERR_URL_SHAPE, lang).to_string());
    }
    Ok(())
}

// --- commands ----------------------------------------------------------------

fn start(app: &mut App, kind: PendingKind, cmd: String, wait_final: Option<std::time::Duration>) {
    let rx = app.session.request_mgmt(cmd, wait_final);
    app.network.pending.push((kind, rx));
}

pub fn action(app: &mut App, entry: NetEntry) {
    sync_lang(app);
    let lang = app.network.lang;
    match entry {
        NetEntry::Ssid | NetEntry::Password | NetEntry::Webdav => {}
        NetEntry::Result(index) => {
            if let Some(result) = app.network.scan.get(index) {
                let ssid = result.ssid.clone();
                app.network.ssid.set(ssid.clone());
                app.network.feedback = tr!(t(NET_FEEDBACK_SELECTED, lang), ssid);
                // Move the selection back to the password field for typing.
                let items = entries(app);
                if let Some(pos) = items.iter().position(|e| *e == NetEntry::Password) {
                    app.network.selection = pos;
                }
            }
        }
        NetEntry::Scan => {
            if !app.network.can_wifi_scan(app) {
                gate(app);
                return;
            }
            // One sweep at a time. Live `@scan result` events are only folded
            // in while `scan_running` holds (`apply_scan_lines`), and the first
            // scan's reply clears that flag — a second scan started meanwhile
            // would then have every one of its results dropped and finish with
            // an empty list.
            if app.network.scan_running
                || app
                    .network
                    .pending
                    .iter()
                    .any(|(kind, _)| matches!(kind, PendingKind::Scan))
            {
                app.network.feedback = t(NET_FEEDBACK_SCANNING, lang).to_string();
                return;
            }
            app.network.scan.clear();
            app.network.selection = app.network.selection.min(clamp_limit(app));
            app.network.scan_running = true;
            app.network.feedback = t(NET_FEEDBACK_SCANNING, lang).to_string();
            start(
                app,
                PendingKind::Scan,
                "@w scan".to_string(),
                Some(SCAN_WAIT),
            );
        }
        NetEntry::WifiConnect => {
            if !app.network.can_wifi(app) {
                gate(app);
                return;
            }
            let ssid = app.network.ssid.as_str().to_string();
            let password = app.network.password.as_str().to_string();
            if let Err(err) =
                validate_ssid(&ssid, lang).and_then(|_| validate_password(&password, lang))
            {
                app.toast(NoticeLevel::Error, err);
                return;
            }
            app.network.feedback = tr!(t(NET_FEEDBACK_CONNECTING, lang), ssid);
            start(
                app,
                PendingKind::WifiConnect,
                format!("@w={ssid},{password}"),
                Some(SCAN_WAIT),
            );
        }
        NetEntry::WifiOff => {
            if !app.network.can_wifi(app) {
                gate(app);
                return;
            }
            app.network.feedback = t(NET_FEEDBACK_WIFI_OFF, lang).to_string();
            start(
                app,
                PendingKind::WifiOff,
                "@w off".to_string(),
                Some(SCAN_WAIT),
            );
        }
        NetEntry::WifiStatus => {
            if !app.network.can_wifi(app) {
                gate(app);
                return;
            }
            start(app, PendingKind::WifiStatus, "@w?".to_string(), None);
        }
        NetEntry::WebdavSet => {
            if !app.network.can_webdav(app) {
                gate(app);
                return;
            }
            let url = app.network.webdav.as_str().to_string();
            if let Err(err) = validate_webdav_url(&url, lang) {
                app.toast(NoticeLevel::Error, err);
                return;
            }
            start(app, PendingKind::WebdavSet, format!("@d={url}"), None);
        }
        NetEntry::WebdavOff => {
            if !app.network.can_webdav(app) {
                gate(app);
                return;
            }
            start(app, PendingKind::WebdavOff, "@d off".to_string(), None);
        }
        NetEntry::WebdavStatus => {
            if !app.network.can_webdav(app) {
                gate(app);
                return;
            }
            start(app, PendingKind::WebdavStatus, "@d?".to_string(), None);
        }
    }
}

fn gate(app: &mut App) {
    app.toast(NoticeLevel::Warn, t(NET_GATE_BLE, app.lang()).to_string());
}

/// Drain finished requests (called once per loop iteration). The oneshot
/// result is consumed exactly once and handed to [`handle_reply`].
pub fn poll(app: &mut App) {
    sync_lang(app);
    let pending = std::mem::take(&mut app.network.pending);
    for (kind, mut rx) in pending {
        match rx.try_recv() {
            Ok(result) => handle_reply(app, kind, Ok(result)),
            Err(oneshot::error::TryRecvError::Empty) => app.network.pending.push((kind, rx)),
            Err(err) => handle_reply(app, kind, Err(err)),
        }
    }
}

/// `@scan error` or the web client's `/^ERR\b/i` finish cases.
fn is_scan_error(line: &str) -> bool {
    if line == "@scan error" {
        return true;
    }
    let Some(head) = line.get(..3) else {
        return false;
    };
    if !head.eq_ignore_ascii_case("ERR") {
        return false;
    }
    // `\b` after "ERR": end of string, or a non-word character next.
    match line[3..].chars().next() {
        None => true,
        Some(next) => !next.is_alphanumeric() && next != '_',
    }
}

/// One row per SSID, keeping the strongest reading (`scanWifi` upserts the
/// same way instead of appending duplicates).
fn upsert_scan(list: &mut Vec<ScanResult>, incoming: ScanResult) {
    let Some(existing) = list.iter_mut().find(|item| item.ssid == incoming.ssid) else {
        list.push(incoming);
        return;
    };
    if let Some(rssi) = incoming.rssi {
        match existing.rssi {
            Some(current) if rssi <= current => {}
            _ => existing.rssi = Some(rssi),
        }
    }
}

/// Fold live `@scan result` events into the form (web `handleWifiScanLine`).
/// Results stream in *while* the request waits for `@scan done`, so reading
/// only the reply — the bare type-2 ack — always ends up with 0 rows.
fn apply_scan_lines(state: &mut NetworkState, lines: &[String]) {
    if !state.scan_running {
        return;
    }
    let lang = state.lang;
    let mut settled: Option<bool> = None; // Some(failed)
    for line in lines {
        let line = line.trim();
        if line == "@scan done" {
            settled.get_or_insert(false);
        } else if is_scan_error(line) {
            settled = Some(true);
        } else if let Some(result) = parse_scan_line(line) {
            upsert_scan(&mut state.scan, result);
        }
    }
    if let Some(failed) = settled {
        state.scan_running = false;
        state.feedback = if failed {
            t(NET_FEEDBACK_SCAN_FAILED, lang).to_string()
        } else {
            tr!(t(NET_FEEDBACK_FOUND, lang), state.scan.len())
        };
    }
}

/// Bus entry point for the live scan stream (CLI `pump_until` equivalent).
pub fn on_mgmt_event(app: &mut App, lines: &[String]) {
    sync_lang(app);
    apply_scan_lines(&mut app.network, lines);
    // Rows were just added under the selection: keep the `▸` on a real row.
    app.network.selection = app.network.selection.min(clamp_limit(app));
}

/// Point the LAN host form at the IP the bridge just confirmed. Returns true
/// when anything moved, so the caller writes the settings file on a real
/// change and not on every status press. Split out from the reply handler so
/// the test can pin the copy without rewriting the developer's `tui.json`.
fn aim_host(app: &mut App, ip: &str) -> bool {
    // `wifi.ip` arrives verbatim from the bridge. The web copies it into the
    // LAN host field only **when it looks like an address** — the rule
    // `prefill_lan_host` already enforces on the other path (`mod.rs` spells
    // it out) — and writing it unconditionally also persisted it, so a bogus
    // or hostile reply could aim the next LAN dial anywhere.
    if !super::diagnostics_view::looks_like_ipv4(ip) {
        return false;
    }
    if app.settings.last_lan_host == ip {
        // Nothing moved. Stomping the field anyway would throw away whatever
        // the user is halfway through typing (a status press can land up to
        // five seconds after they started).
        return false;
    }
    app.sidebar.lan_host.set(ip.to_string());
    app.settings.last_lan_host = ip.to_string();
    // Tokens are stored per host: leaving the previous host's token in the
    // field would make the very next LAN dial authenticate against the new IP
    // and be rejected. The sidebar does the same when the host is edited by
    // hand.
    let store = crate::lan_token_store::TokenStore::load();
    super::sidebar::fill_token_from_store(app, &store);
    true
}

/// Largest row index the current list holds. The row list changes size when a
/// scan clears it and again as results stream in, so a selection carried across
/// either boundary points at nothing: no `▸` is drawn, Enter and typing do
/// nothing (`items.get(...)` returns `None`) — and once the list grows back
/// the *wrong* row activates, a scan result where WebDAV was selected.
fn clamp_limit(app: &App) -> usize {
    entries(app).len().saturating_sub(1)
}

fn handle_reply(
    app: &mut App,
    kind: PendingKind,
    outcome: Result<Result<MgmtReply, String>, oneshot::error::TryRecvError>,
) {
    let lang = app.lang();
    if kind == PendingKind::Scan {
        app.network.scan_running = false;
    }
    let reply = match outcome {
        Ok(Ok(reply)) => reply,
        Ok(Err(err)) => {
            // Same treatment as the reply toasts below: this text is built
            // from the device's failure line and lands in the permanent
            // notice log as well as on screen.
            app.toast(NoticeLevel::Error, super::replies::redact_secrets(&err));
            app.network.feedback = t(NET_FEEDBACK_REQUEST_FAILED, lang).to_string();
            return;
        }
        Err(oneshot::error::TryRecvError::Closed) => {
            app.toast(
                NoticeLevel::Error,
                t(NET_FEEDBACK_REQUEST_CANCELLED, lang).to_string(),
            );
            app.network.feedback = t(NET_FEEDBACK_REQUEST_FAILED, lang).to_string();
            return;
        }
        Err(oneshot::error::TryRecvError::Empty) => return,
    };
    let mut text = String::new();
    for line in reply.lines.iter().chain(reply.events.iter()) {
        text.push_str(line);
        text.push('\n');
    }

    match kind {
        PendingKind::Scan => {
            // The results already streamed in as live events (see
            // `on_mgmt_event`); the reply only re-merges whatever it carries
            // and settles the count. Never replace the list here: that wiped
            // the rows the radio had already reported.
            let mut failed = false;
            for line in text.lines() {
                let line = line.trim();
                if is_scan_error(line) {
                    failed = true;
                } else if let Some(result) = parse_scan_line(line) {
                    upsert_scan(&mut app.network.scan, result);
                }
            }
            app.network.selection = app.network.selection.min(clamp_limit(app));
            app.network.feedback = if failed {
                t(NET_FEEDBACK_SCAN_FAILED, lang).to_string()
            } else {
                tr!(t(NET_FEEDBACK_FOUND, lang), app.network.scan.len())
            };
        }
        PendingKind::WifiConnect | PendingKind::WifiOff => {
            if let Some(status) = parse_wifi_reply(&text) {
                app.network.feedback = tr!(
                    t(NET_FEEDBACK_WIFI, lang),
                    wifi_state_text(&status.state, lang)
                );
                app.network.wifi_status = Some(status.clone());
            } else {
                app.network.feedback = t(NET_FEEDBACK_WIFI_SENT, lang).to_string();
            }
        }
        PendingKind::WifiStatus => match parse_wifi_reply(&text) {
            Some(status) => {
                let state = wifi_state_text(&status.state, lang);
                app.network.feedback = if status.ip.is_empty() {
                    tr!(t(NET_FEEDBACK_WIFI, lang), state)
                } else {
                    tr!(t(NET_FEEDBACK_WIFI_IP, lang), state, &status.ip)
                };
                // A confirmed IP is already the address the LAN transport
                // dials, so pressing the WiFi status also points the host
                // field at it. The value was on screen one line above; making
                // the user retype it into *局域网主机* was pure friction, and
                // without it the field kept whatever stale address it was
                // last handed.
                if !status.ip.is_empty() && aim_host(app, &status.ip) {
                    if let Err(err) = super::settings::save(&app.settings) {
                        app.toast(NoticeLevel::Warn, tr!(t(MSG_SAVE_SETTINGS, lang), err));
                    }
                }
                app.network.wifi_status = Some(status);
            }
            None => {
                app.toast(
                    NoticeLevel::Warn,
                    super::replies::redact_secrets(text.trim()),
                );
            }
        },
        PendingKind::WebdavSet | PendingKind::WebdavOff | PendingKind::WebdavStatus => {
            match parse_webdav_reply(&text) {
                Some(status) => {
                    app.network.feedback = tr!(
                        t(NET_FEEDBACK_WEBDAV, lang),
                        webdav_state_text(&status.state, lang)
                    );
                    app.network.webdav_status = Some(status);
                }
                None => {
                    app.toast(
                        NoticeLevel::Warn,
                        super::replies::redact_secrets(text.trim()),
                    );
                }
            }
        }
    }
}

// --- keys --------------------------------------------------------------------

pub fn handle_key(app: &mut App, key: KeyEvent) {
    app.network.selection = app.network.selection.min(clamp_limit(app));
    let items = entries(app);
    let len = items.len();
    match key.code {
        KeyCode::Up => app.network.selection = app.network.selection.saturating_sub(1),
        KeyCode::Down => {
            app.network.selection = (app.network.selection + 1).min(len.saturating_sub(1))
        }
        KeyCode::Tab => {
            app.network.selection = (app.network.selection + 1).min(len.saturating_sub(1))
        }
        KeyCode::BackTab => app.network.selection = app.network.selection.saturating_sub(1),
        KeyCode::Home => app.network.selection = 0,
        KeyCode::End => app.network.selection = len.saturating_sub(1),
        KeyCode::Esc => app.focus = Focus::Center,
        KeyCode::Enter => {
            if let Some(entry) = items.get(app.network.selection).copied() {
                match entry {
                    NetEntry::Ssid | NetEntry::Password | NetEntry::Webdav => {
                        // Enter in a field advances to the next row.
                        app.network.selection = (app.network.selection + 1).min(len - 1);
                    }
                    other => action(app, other),
                }
            }
        }
        KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            let Some(entry) = items.get(app.network.selection).copied() else {
                return;
            };
            match entry {
                NetEntry::Ssid => app.network.ssid.insert_char(c),
                NetEntry::Password => app.network.password.insert_char(c),
                NetEntry::Webdav => app.network.webdav.insert_char(c),
                // Quick toggles when no field is selected.
                NetEntry::Scan if c == 's' => action(app, NetEntry::Scan),
                NetEntry::WifiStatus if c == 'q' => action(app, NetEntry::WifiStatus),
                _ => {}
            }
        }
        KeyCode::Backspace => {
            let Some(entry) = items.get(app.network.selection).copied() else {
                return;
            };
            match entry {
                NetEntry::Ssid => app.network.ssid.backspace(),
                NetEntry::Password => app.network.password.backspace(),
                NetEntry::Webdav => app.network.webdav.backspace(),
                _ => {}
            }
        }
        KeyCode::Left => {
            let Some(entry) = items.get(app.network.selection).copied() else {
                return;
            };
            match entry {
                NetEntry::Ssid => app.network.ssid.left(),
                NetEntry::Password => app.network.password.left(),
                NetEntry::Webdav => app.network.webdav.left(),
                _ => {}
            }
        }
        KeyCode::Right => {
            let Some(entry) = items.get(app.network.selection).copied() else {
                return;
            };
            match entry {
                NetEntry::Ssid => app.network.ssid.right(),
                NetEntry::Password => app.network.password.right(),
                NetEntry::Webdav => app.network.webdav.right(),
                _ => {}
            }
        }
        KeyCode::Char('v') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            // Reveal / mask the password (`Ctrl+P` belongs to the palette).
            app.network.show_password = !app.network.show_password;
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_app;
    use super::*;

    /// Pressing the WiFi status with a confirmed IP has to leave the LAN host
    /// field holding that IP: the number was printed one line above, and the
    /// field is what the LAN transport actually dials. Before this the value
    /// only ever reached the screen, and the form kept a stale address.
    ///
    /// Only loopback and documentation addresses here (RFC 5737 `192.0.2.0/24`
    ///): a test is not the place to bake our own LAN's numbering into the
    /// source, where it reads like a shipped default.
    #[test]
    fn a_confirmed_wifi_ip_points_the_lan_host_field_at_it() {
        let mut app = test_app();
        // What the form still holds when the radio has nothing to say yet.
        app.sidebar.lan_host.set("127.0.0.1".to_string());
        app.settings.last_lan_host = "127.0.0.1".to_string();

        assert!(aim_host(&mut app, "192.0.2.1"));
        assert_eq!(app.sidebar.lan_host.as_str(), "192.0.2.1");
        assert_eq!(app.settings.last_lan_host, "192.0.2.1");

        // The same IP again is not a change: nothing to persist, so the
        // settings file is not rewritten on every status press.
        assert!(!aim_host(&mut app, "192.0.2.1"));
        assert_eq!(app.sidebar.lan_host.as_str(), "192.0.2.1");
    }

    /// P2: the same seed lands on a field that still holds a **longer** pasted
    /// host. `aim_host` used to assign `lan_host.text` directly, leaving the
    /// caret at the end of the old value — the next frame then sliced past the
    /// end of the new one and the TUI aborted. It now goes through `set()`.
    #[test]
    fn aiming_a_short_ip_at_a_long_pasted_host_keeps_the_form_renderable() {
        let mut app = test_app();
        let pasted = "ws://192.0.2.9:99999/ws";
        app.sidebar.lan_host.set(pasted);
        app.settings.last_lan_host = pasted.to_string();

        assert!(aim_host(&mut app, "192.0.2.1"));
        assert_eq!(app.sidebar.lan_host.as_str(), "192.0.2.1");
        // Rendered text plus caret column, exactly as the sidebar row asks.
        assert_eq!(
            app.sidebar.lan_host.display(None),
            ("192.0.2.1".to_string(), 9)
        );
    }

    #[test]
    fn accessory_limits_match_the_web_client() {
        assert_eq!(SSID_MAX, 32);
        assert_eq!(PASSWORD_MAX, 64);
        assert_eq!(WEBDAV_URL_MAX, 256);
    }

    /// Regression: the results arrive as live events *before* `@scan done`,
    /// while the reply itself only carries the type-2 ack. Reading the reply
    /// alone showed "Found 0 networks." no matter what the radio reported.
    #[test]
    fn live_scan_events_fill_the_list() {
        let mut state = NetworkState::new();
        state.scan_running = true;
        apply_scan_lines(
            &mut state,
            &[
                // Field order as the firmware emits it (`src/wifi.c`:
                // `@scan result %.*s %s ch=%u %ddBm`) — the RSSI sits at the
                // end, which is where the web's pattern looks for it.
                "@scan result HomeWiFi WPA2 ch=6 -50 dBm".to_string(),
                "@scan result CoffeeShop open ch=1 -70 dBm".to_string(),
                "@scan done".to_string(),
            ],
        );
        assert!(!state.scan_running, "`@scan done` must settle the scan");
        assert_eq!(state.scan.len(), 2);
        assert_eq!(state.scan[0].ssid, "HomeWiFi");
        assert_eq!(state.scan[0].rssi, Some(-50));
        assert_eq!(state.scan[0].channel, Some(6));
        // The web lowercases the token when it matches
        // (`web/app.js` → `securityMatch[1].toLowerCase()`), and the spec
        // spells the set lowercase too, so the row shows `wpa2`.
        assert_eq!(state.scan[0].security.as_deref(), Some("wpa2"));
        assert_eq!(state.feedback, "Found 2 networks.");
    }

    #[test]
    fn duplicate_ssids_keep_the_strongest_reading() {
        let mut state = NetworkState::new();
        state.scan_running = true;
        apply_scan_lines(
            &mut state,
            &[
                "@scan result HomeWiFi ch=6 -80 dBm".to_string(),
                "@scan result HomeWiFi ch=11 -40 dBm".to_string(),
                "@scan done".to_string(),
            ],
        );
        assert_eq!(state.scan.len(), 1, "one row per SSID");
        assert_eq!(state.scan[0].rssi, Some(-40), "the strongest reading wins");
    }

    /// The old handler set "Scan failed." inside the loop and then
    /// unconditionally overwrote it with "Found 0 networks.".
    #[test]
    fn a_failed_scan_says_so() {
        let mut state = NetworkState::new();
        state.scan_running = true;
        apply_scan_lines(&mut state, &["@scan error".to_string()]);
        assert_eq!(state.feedback, "Scan failed.");
        assert!(!state.scan_running);

        let mut state = NetworkState::new();
        state.scan_running = true;
        apply_scan_lines(&mut state, &["ERR timeout".to_string()]);
        assert_eq!(state.feedback, "Scan failed.");
    }

    #[test]
    fn stray_events_never_touch_an_idle_form() {
        let mut state = NetworkState::new(); // scan_running == false
        apply_scan_lines(&mut state, &["@scan result Ghost -1 dBm".to_string()]);
        assert!(state.scan.is_empty());
        assert_eq!(state.feedback, "Scan to select a nearby 2.4 GHz network.");
    }

    /// `finishScan(true)` triggers: `/^ERR\b/i` or exactly `@scan error`.
    #[test]
    fn error_line_rules_match_the_web_regex() {
        for line in ["ERR timeout", "err failed", "ERR", "@scan error"] {
            assert!(is_scan_error(line), "{line} must end the scan as a failure");
        }
        for line in [
            "ERROR",
            "no error here",
            "@scan done",
            "ok",
            "@scan result x",
        ] {
            assert!(!is_scan_error(line), "{line} is not a scan error");
        }
        // Multi-byte input must not panic on a byte-boundary slice.
        assert!(!is_scan_error("错误"));
    }

    #[test]
    fn ssid_rules() {
        assert!(validate_ssid("MyNet", Lang::En).is_ok());
        assert!(validate_ssid("", Lang::En).is_err());
        assert!(validate_ssid(&"x".repeat(33), Lang::En).is_err());
        assert!(validate_ssid("a,b", Lang::En).is_err());
        assert!(validate_ssid("bad\tname", Lang::En).is_err());
        assert_eq!(
            validate_ssid(&"x".repeat(33), Lang::En).unwrap_err(),
            "A network name is limited to 32 characters."
        );
        assert_eq!(validate_ssid("", Lang::Zh).unwrap_err(), "请输入网络名称。");
    }

    #[test]
    fn password_and_url_rules() {
        assert!(validate_password("", Lang::En).is_ok());
        assert!(validate_password("secret", Lang::En).is_ok());
        assert!(validate_password(&"x".repeat(65), Lang::En).is_err());
        assert!(validate_password("bad\npass", Lang::En).is_err());
        assert!(validate_webdav_url("http://host/dav/", Lang::En).is_ok());
        assert!(validate_webdav_url("https://host/d", Lang::En).is_ok());
        assert_eq!(
            validate_webdav_url("ftp://host", Lang::En).unwrap_err(),
            "The WebDAV URL must start with http:// or https://."
        );
        assert!(validate_webdav_url("", Lang::En).is_err());
        assert!(validate_webdav_url("http://h/o p", Lang::En).is_err());
        assert!(validate_webdav_url(&format!("http://h/{}", "x".repeat(300)), Lang::En).is_err());
        assert_eq!(
            validate_webdav_url("ftp://host", Lang::Zh).unwrap_err(),
            "WebDAV URL 必须以 http:// 或 https:// 开头。"
        );
    }

    /// A scan list taller than the pane is the case the two mechanisms exist
    /// for: `center_scroll` and `network.selection` were written by different
    /// keys and neither read the other, so the `▸` could sit entirely outside
    /// the window the frame was showing.
    fn long_list(height: u16) -> App {
        let mut app = crate::tui::test_app();
        app.network.scan = (0..12).map(|i| scan(&format!("Net{i:02}"))).collect();
        app.center_height = height;
        app.center_scroll = 0;
        assert!(
            render_lines(&app).len() > height as usize * 2,
            "list outgrows the pane"
        );
        app
    }

    /// The cursor walked off the bottom: the window comes to it.
    #[test]
    fn a_cursor_that_would_leave_the_pane_drags_the_window_with_it() {
        let mut app = long_list(6);
        app.network.selection = entries(&app).len() - 1;
        assert!(!selection_visible(&app), "starts off screen");

        follow_selection(&mut app);

        assert!(
            selection_visible(&app),
            "scroll {} shows the row the cursor is on",
            app.center_scroll
        );
    }

    /// PageUp/PageDown only ever wrote `center_scroll`, so the row the cursor
    /// named stayed where the window had left it.
    #[test]
    fn a_window_scrolled_away_takes_the_cursor_with_it() {
        let mut app = long_list(6);
        app.network.selection = 0;
        app.center_scroll = 6;
        assert!(!selection_visible(&app), "the cursor is above the window");

        follow_scroll(&mut app);

        assert!(
            selection_visible(&app),
            "selection {} scroll {}",
            app.network.selection,
            app.center_scroll
        );
    }

    /// Below the last row there is nothing to follow — just the feedback line
    /// and the key hint — so the window goes back to the cursor rather than
    /// leaving it stranded.
    #[test]
    fn a_window_past_every_row_comes_back_to_the_cursor() {
        let mut app = long_list(2);
        let lines = render_lines(&app).len();
        app.center_scroll = (lines - 2) as u16;
        app.network.selection = 0;
        assert!(!selection_visible(&app), "the window shows the tail only");

        follow_scroll(&mut app);

        assert_eq!(app.network.selection, 0, "nothing to follow, nothing moves");
        assert!(
            selection_visible(&app),
            "scroll {} shows the row the cursor is on",
            app.center_scroll
        );
    }

    #[test]
    fn network_state_defaults_need_no_tty() {
        let state = NetworkState::new();
        assert!(state.scan.is_empty());
        assert!(!state.scan_running);
        assert!(state.feedback.contains("Scan to select"));
        assert_eq!(state.wifi_status, None);
        assert_eq!(NetEntry::Result(3), NetEntry::Result(3));
    }

    /// The feedback string is written in the state's language, and the standing
    /// line is re-rendered when the session language lands.
    #[test]
    fn feedback_follows_the_language() {
        let mut state = NetworkState::new();
        state.lang = Lang::Zh;
        state.scan_running = true;
        apply_scan_lines(
            &mut state,
            &[
                "@scan result HomeWiFi -50 dBm".to_string(),
                "@scan done".to_string(),
            ],
        );
        assert_eq!(state.feedback, "找到 1 个网络。");

        let mut state = NetworkState::new();
        state.lang = Lang::Zh;
        state.scan_running = true;
        apply_scan_lines(&mut state, &["@scan error".to_string()]);
        assert_eq!(state.feedback, "扫描失败。");
    }

    #[test]
    fn every_network_message_is_translated() {
        super::super::i18n::assert_bilingual(ALL);
        assert!(ALL.len() >= 40, "the network view carries 40 messages");
    }

    /// ↑/↓ walk `entries()` while the eye reads `render_lines`, so the row the
    /// cursor lands on has to move down the screen with every step. Regression:
    /// the scan results used to be selectable *after* the WebDAV block while
    /// being drawn *above* it, so the cursor jumped from the last WiFi button
    /// straight to WebDAV and only reached the results one step later.
    #[test]
    fn the_cursor_walks_the_rows_top_to_bottom() {
        let mut app = crate::tui::test_app();
        app.network.scan = vec![
            ScanResult {
                ssid: "Alpha".to_string(),
                rssi: Some(-40),
                channel: Some(1),
                security: None,
            },
            ScanResult {
                ssid: "Beta".to_string(),
                rssi: Some(-70),
                channel: Some(6),
                security: None,
            },
        ];
        let items = entries(&app);
        assert!(
            items.len() > 10,
            "the scan results must be selectable rows too"
        );
        let mut previous = 0;
        for index in 0..items.len() {
            app.network.selection = index;
            let row = render_lines(&app)
                .iter()
                .position(|line| line.spans.first().map(|span| span.content.as_ref()) == Some("▸ "))
                .unwrap_or_else(|| panic!("row {index} draws no cursor marker"));
            assert!(
                row > previous,
                "row {index} is drawn at screen line {row}, at or above the \
                 previous entry at line {previous}"
            );
            previous = row;
        }
    }

    /// K7 end to end: the wire line, the fold into the list and the row the
    /// eye reads. An SSID with spaces survives all three — the parser took
    /// the first whitespace-delimited token, so `My Home Network` reached the
    /// screen as `My`.
    #[test]
    fn a_scan_result_with_spaces_in_the_ssid_draws_whole() {
        let mut state = NetworkState::new();
        state.scan_running = true;
        // Field order as the firmware emits it (`src/wifi.c`:
        // `@scan result %.*s %s ch=%u %ddBm`).
        apply_scan_lines(
            &mut state,
            &["@scan result My Home Network wpa2 ch=6 -48dBm".to_string()],
        );
        assert_eq!(state.scan.len(), 1, "the line is one network");
        assert_eq!(state.scan[0].ssid, "My Home Network");
        assert_eq!(state.scan[0].rssi, Some(-48));

        let mut app = crate::tui::test_app();
        app.network = state;
        let rows: Vec<String> = render_lines(&app)
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect();
        assert!(
            rows.iter().any(|row| row.contains("My Home Network")),
            "the scan row must draw the whole SSID, drew {rows:?}"
        );
    }

    /// One sweep at a time: the first scan's reply clears `scan_running`, and
    /// every live `@scan result` that arrives after that is dropped
    /// (`apply_scan_lines` returns early). A second scan started in the window
    /// therefore has all of its results swallowed and finishes with an empty
    /// list under a "Found 0 networks." count.
    #[test]
    fn a_second_scan_cannot_start_while_the_first_is_running() {
        let mut app = test_app();
        app.info.kind = Some(crate::transport::TransportKind::Ble);
        app.info.capabilities = MGMT_CAP_WIFI | MGMT_CAP_ASYNC_EVENTS;

        action(&mut app, NetEntry::Scan);
        assert!(app.network.scan_running, "the sweep is running");
        assert_eq!(app.network.pending.len(), 1);

        action(&mut app, NetEntry::Scan);
        assert_eq!(
            app.network.pending.len(),
            1,
            "the second sweep is refused instead of shadowing the first"
        );
    }

    /// The row list shrinks when a scan clears it and the selection was never
    /// clamped on that path: it then points past the end, so no `▸` is drawn
    /// and Enter/typing do nothing until an arrow key is pressed.
    #[test]
    fn clearing_the_scan_list_keeps_the_selection_on_a_real_row() {
        let mut app = test_app();
        app.info.kind = Some(crate::transport::TransportKind::Ble);
        app.info.capabilities = MGMT_CAP_WIFI | MGMT_CAP_ASYNC_EVENTS;
        app.network.scan = vec![scan("One"), scan("Two")];
        app.network.selection = entries(&app).len() - 1; // `End` on the long list
        assert!(app.network.selection > 8, "results push the rows down");

        action(&mut app, NetEntry::Scan); // clears the list

        assert!(
            app.network.selection < entries(&app).len(),
            "selection {} points past the {} rows",
            app.network.selection,
            entries(&app).len()
        );
    }

    /// `wifi.ip` comes from the bridge. The web copies it into the LAN host
    /// field only when it looks like an address — and this value is also what
    /// gets persisted to `tui.json`, so it must not be aimable from outside.
    #[test]
    fn a_bogus_wifi_ip_never_reaches_the_lan_host_field() {
        let mut app = test_app();
        app.sidebar.lan_host.set("172.20.10.7".to_string());
        app.settings.last_lan_host = "172.20.10.7".to_string();

        for bogus in ["", "not-an-ip", "999.1.1.1", "192.168.1.5; reboot"] {
            assert!(!aim_host(&mut app, bogus), "{bogus:?} must be refused");
            assert_eq!(app.sidebar.lan_host.as_str(), "172.20.10.7");
            assert_eq!(app.settings.last_lan_host, "172.20.10.7");
        }
    }

    /// A status press can land up to five seconds after the user started
    /// typing a host: when the address did not change, the field is left
    /// alone rather than rewritten under the caret.
    #[test]
    fn a_status_reply_leaves_a_half_typed_host_alone() {
        let mut app = test_app();
        app.settings.last_lan_host = "192.168.1.7".to_string();
        app.sidebar.lan_host.set("10.0.0.5".to_string()); // the user is typing

        assert!(!aim_host(&mut app, "192.168.1.7"));
        assert_eq!(
            app.sidebar.lan_host.as_str(),
            "10.0.0.5",
            "the half-typed host survives a reply that changed nothing"
        );
    }

    fn scan(ssid: &str) -> crate::tui::replies::ScanResult {
        crate::tui::replies::ScanResult {
            ssid: ssid.to_string(),
            rssi: None,
            channel: None,
            security: None,
        }
    }
}
