//! Protocol codecs: management framing, reliable UART framing, terminal
//! geometry sync and argument validation. Pure logic, no I/O.

pub mod geometry;
pub mod mgmt;
pub mod uart;
pub mod validate;

pub use geometry::{
    looks_like_shell_prompt, terminal_geometry, terminal_geometry_command, TerminalGeometrySync,
};
pub use mgmt::{display_command, json_record, plain_line, MgmtCore, MgmtError, MgmtReply};
pub use uart::{UartCodec, UartGap};
pub use validate::{
    describe_escape, match_device, normalize_name_prefix, normalize_uart_spec, parse_escape,
    python_repr_bytes, resolve_wifi_credentials, translate_enter,
};
