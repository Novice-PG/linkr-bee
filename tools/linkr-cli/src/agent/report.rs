//! Report export: port of `web/agent_report.js`.
//!
//! One Markdown document per task: what was asked, what the assistant planned,
//! what was actually sent to the target, what came back, and what is durable
//! about the device. Everything passes through the same redaction as the task
//! store and the size is capped.

use super::memory::{redact_task_text, Note, Task};

/// Absolute cap; longer reports end in `[truncated]`.
pub const REPORT_MAX_CHARS: usize = 200_000;
/// Evidence is quoted as at most 2000 characters, after `redactTaskText`
/// already capped it (`web/agent_report.js`).
pub const REPORT_EVIDENCE_CHARS: usize = 2_000;

#[derive(Debug, Clone, Default)]
pub struct ReportDownload {
    pub path: String,
    pub bytes: Option<u64>,
    pub sha256: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ReportRecord {
    pub id: String,
    pub payload: String,
    pub delivery: String,
    pub execution_status: String,
    pub exit_code: Option<i64>,
    pub observation: String,
    pub error: Option<String>,
    pub download: Option<ReportDownload>,
    pub evidence: Option<String>,
}

#[derive(Debug)]
pub struct ReportInput<'a> {
    pub lang: &'a str,
    pub device: &'a str,
    pub task: Option<&'a Task>,
    pub tasks: &'a [Task],
    pub records: &'a [ReportRecord],
    pub notes: &'a [Note],
    /// ISO timestamp override; `None` uses the current time.
    pub generated_at: Option<&'a str>,
}

/// `parts.filter(present).join(" · ")`.
fn line(parts: &[Option<String>]) -> String {
    parts
        .iter()
        .filter_map(|part| part.as_ref())
        .cloned()
        .collect::<Vec<_>>()
        .join(" · ")
}

fn present(value: &str) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

/// A serial payload or a log excerpt carries CR/LF and escape bytes that would
/// break the Markdown layout; keep the text, drop the control characters.
fn printable(text: &str) -> String {
    text.chars()
        .filter(|c| {
            let code = *c as u32;
            !((0x00..=0x08).contains(&code)
                || (0x0b..=0x1f).contains(&code)
                || code == 0x7f
                || *c == '\r')
        })
        .collect()
}

fn tail_chars(text: &str, limit: usize) -> String {
    let count = text.chars().count();
    if count <= limit {
        return text.to_string();
    }
    text.chars().skip(count - limit).collect()
}

fn iso_from_ms(ms: u64) -> String {
    chrono::DateTime::from_timestamp_millis(ms as i64)
        .map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        .unwrap_or_default()
}

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn plan_section(task: Option<&Task>, zh: bool) -> Vec<String> {
    let plan = match task {
        Some(task) if !task.plan.is_empty() => &task.plan,
        _ => return Vec::new(),
    };
    let mut out = vec![
        if zh { "## 计划" } else { "## Plan" }.to_string(),
        String::new(),
    ];
    for step in plan {
        out.push(
            format!(
                "- [{}] {}",
                if step.status.is_empty() {
                    "pending"
                } else {
                    &step.status
                },
                redact_task_text(&step.title)
            )
            .trim_end()
            .to_string(),
        );
        if !step.verification.is_empty() {
            out.push(format!(
                "  - {}: {}",
                if zh { "验证" } else { "Verification" },
                redact_task_text(&step.verification)
            ));
        }
        if !step.next_action.is_empty() {
            out.push(format!(
                "  - {}: {}",
                if zh { "下一步" } else { "Next" },
                redact_task_text(&step.next_action)
            ));
        }
    }
    out.push(String::new());
    out
}

fn execution_section(records: &[ReportRecord], zh: bool) -> Vec<String> {
    if records.is_empty() {
        return Vec::new();
    }
    let mut out = vec![
        if zh {
            "## 串口执行记录"
        } else {
            "## Target executions"
        }
        .to_string(),
        String::new(),
    ];
    for record in records {
        out.push(format!(
            "### {}",
            if record.id.is_empty() {
                "execution"
            } else {
                &record.id
            }
        ));
        out.push(line(&[
            Some(if zh { "命令" } else { "Command" }.to_string()),
            Some(format!(
                "`{}`",
                printable(&redact_task_text(&record.payload))
            )),
        ]));
        let exit = record
            .exit_code
            .map(|code| format!("{} {}", if zh { "退出码" } else { "exit code" }, code));
        out.push(line(&[
            Some(if zh { "投递" } else { "Delivery" }.to_string()),
            present(&record.delivery),
            present(&record.execution_status),
            exit,
            present(&record.observation),
        ]));
        if let Some(error) = &record.error {
            out.push(line(&[
                Some(if zh { "错误" } else { "Error" }.to_string()),
                Some(redact_task_text(error)),
            ]));
        }
        if let Some(download) = &record.download {
            let bytes = download.bytes.map(|value| format!("{} bytes", value));
            let sha = download
                .sha256
                .as_ref()
                .map(|value| format!("SHA-256 {}", value));
            out.push(line(&[
                Some(if zh { "下载" } else { "Download" }.to_string()),
                Some(redact_task_text(&download.path)),
                bytes,
                sha,
            ]));
        }
        if let Some(evidence) = &record.evidence {
            let bounded = tail_chars(
                &printable(&redact_task_text(evidence)),
                REPORT_EVIDENCE_CHARS,
            );
            out.push(String::new());
            out.push("```text".to_string());
            out.push(bounded);
            out.push("```".to_string());
        }
        out.push(String::new());
    }
    out
}

/// Build the export for one task.
pub fn build_report(input: &ReportInput<'_>) -> String {
    let zh = input.lang.to_lowercase().starts_with("zh");
    let mut out = vec![
        if zh {
            "# Linkr Bee 排查报告"
        } else {
            "# Linkr Bee diagnostic report"
        }
        .to_string(),
        String::new(),
        line(&[
            Some(if zh { "设备" } else { "Device" }.to_string()),
            Some(redact_task_text(input.device)),
        ]),
        line(&[
            Some(if zh { "生成时间" } else { "Generated" }.to_string()),
            Some(
                input
                    .generated_at
                    .map(|stamp| stamp.to_string())
                    .unwrap_or_else(now_iso),
            ),
        ]),
    ];
    if let Some(task) = input.task {
        if !task.goal.is_empty() {
            out.push(line(&[
                Some(if zh { "目标" } else { "Goal" }.to_string()),
                Some(redact_task_text(&task.goal)),
            ]));
        }
        if !task.status.is_empty() {
            out.push(line(&[
                Some(if zh { "状态" } else { "Status" }.to_string()),
                Some(task.status.clone()),
            ]));
        }
    }
    out.push(String::new());

    if let Some(task) = input.task {
        if !task.summary.is_empty() {
            out.push(if zh {
                "## 助手结论".to_string()
            } else {
                "## Assistant summary".to_string()
            });
            out.push(String::new());
            out.push(redact_task_text(&task.summary));
            out.push(String::new());
        }
    }
    out.extend(plan_section(input.task, zh));
    out.extend(execution_section(input.records, zh));

    if !input.notes.is_empty() {
        out.push(
            if zh {
                "## 设备笔记"
            } else {
                "## Device notes"
            }
            .to_string(),
        );
        out.push(String::new());
        for note in input.notes {
            let mut entry = format!("- {}", redact_task_text(&note.text));
            if !note.evidence.is_empty() {
                entry.push_str(&format!(" ({})", redact_task_text(&note.evidence)));
            }
            out.push(entry);
        }
        out.push(String::new());
    }
    if !input.tasks.is_empty() {
        out.push(if zh {
            "## 该设备的历史任务".to_string()
        } else {
            "## Earlier tasks on this device".to_string()
        });
        out.push(String::new());
        for item in input.tasks {
            // `web/agent_report.js`: `` `- ${redactTaskText(item.goal)} — ${item.status}${updated}` ``
            let mut entry = format!("- {} — {}", redact_task_text(&item.goal), item.status);
            if item.updated_at > 0 {
                entry.push_str(&format!(" ({})", iso_from_ms(item.updated_at)));
            }
            out.push(entry);
        }
        out.push(String::new());
    }
    out.push(
        if zh {
            "> 退出码与助手结论都不等于目标达成；请以报告中的证据片段自行核对。"
        } else {
            "> Exit codes and assistant conclusions are not proof that the goal was met; check the quoted evidence."
        }
        .to_string(),
    );
    let report = out.join("\n");
    let count = report.chars().count();
    if count > REPORT_MAX_CHARS {
        let head: String = report.chars().take(REPORT_MAX_CHARS).collect();
        format!("{}\n\n[truncated]", head)
    } else {
        report
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::memory::{TaskExecution, TaskStep};

    const JS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../web/agent_report.js");

    fn task() -> Task {
        Task {
            id: "t1".into(),
            device_key: "target:abc".into(),
            updated_at: 1_700_000_000_000,
            goal: "reproduce the reboot with password=hunter2".into(),
            summary: "Board reboots under load.".into(),
            status: "blocked".into(),
            plan: vec![TaskStep {
                title: "read serial log".into(),
                status: "completed".into(),
                verification: "panic line observed".into(),
                next_action: String::new(),
            }],
            executions: vec![TaskExecution {
                delivery: "sent".into(),
                exit_code: Some(0),
                execution_status: "completed".into(),
                path: String::new(),
                observation: "prompt-returned".into(),
            }],
        }
    }

    fn record() -> ReportRecord {
        ReportRecord {
            id: "serial-1".into(),
            payload: "sha256sum /tmp/app.bin".into(),
            delivery: "sent".into(),
            execution_status: "completed".into(),
            exit_code: Some(0),
            observation: "prompt-returned".into(),
            error: None,
            download: Some(ReportDownload {
                path: "/tmp/app.bin".into(),
                bytes: Some(1024),
                sha256: Some("a".repeat(64)),
            }),
            evidence: Some("line\n".to_string() + &"e".repeat(5000)),
        }
    }

    fn input<'a>(task: Option<&'a Task>) -> ReportInput<'a> {
        ReportInput {
            lang: "en",
            device: "Linkr Bee AA:BB",
            task,
            tasks: &[],
            records: std::slice::from_ref(Box::leak(Box::new(record()))),
            notes: &[],
            generated_at: Some("2026-10-02T00:00:00.000Z"),
        }
    }

    #[test]
    fn headings_and_footer_follow_agent_report_js() {
        let source = std::fs::read_to_string(JS).expect("read web/agent_report.js");
        assert!(source.contains("# Linkr Bee diagnostic report"));
        assert!(source.contains("# Linkr Bee 排查报告"));
        assert!(source.contains("> Exit codes and assistant conclusions are not proof that the goal was met; check the quoted evidence."));
        assert!(source.contains("REPORT_MAX_CHARS = 200000"));
        assert!(source.contains("REPORT_EVIDENCE_CHARS = 2000"));
        assert_eq!(REPORT_MAX_CHARS, 200_000);
        assert_eq!(REPORT_EVIDENCE_CHARS, 2_000);

        let task = task();
        let notes = vec![Note {
            id: "n1".into(),
            device_key: "k".into(),
            text: "bootloader needs raw Enter".into(),
            evidence: "U-Boot prompt".into(),
            created_at: 1,
            duplicate: false,
        }];
        let mut input = input(Some(&task));
        input.notes = &notes;
        input.tasks = std::slice::from_ref(&task);
        let report = build_report(&input);

        let headings: Vec<&str> = report
            .lines()
            .filter(|line| line.starts_with('#'))
            .collect();
        assert_eq!(
            headings,
            vec![
                "# Linkr Bee diagnostic report",
                "## Assistant summary",
                "## Plan",
                "## Target executions",
                "### serial-1",
                "## Device notes",
                "## Earlier tasks on this device",
            ],
            "section order"
        );
        assert_eq!(
            report.lines().last().unwrap(),
            "> Exit codes and assistant conclusions are not proof that the goal was met; check the quoted evidence."
        );
        // Metadata lines use the " · " separator.
        assert!(report.contains("Device · Linkr Bee AA:BB"));
        assert!(report.contains("Generated · 2026-10-02T00:00:00.000Z"));
        assert!(report.contains("Goal · reproduce the reboot with password=[redacted]"));
        assert!(report.contains("Status · blocked"));
        // Plan lines carry status, verification and next action labels.
        assert!(report.contains("- [completed] read serial log"));
        assert!(report.contains("  - Verification: panic line observed"));
        // Execution block.
        assert!(report.contains("### serial-1"));
        assert!(report.contains("Command · `sha256sum /tmp/app.bin`"));
        assert!(report.contains("Delivery · sent · completed · exit code 0 · prompt-returned"));
        assert!(report.contains(&format!("SHA-256 {}", "a".repeat(64))));
        assert!(report.contains("Download · /tmp/app.bin · 1024 bytes"));
        // Notes and earlier tasks.
        assert!(report.contains("- bootloader needs raw Enter (U-Boot prompt)"));
        assert!(report.contains(&format!(
            "- {} — blocked (2023-11-14T22:13:20.000Z)",
            redact_task_text(&task.goal)
        )));
    }

    #[test]
    fn evidence_is_capped_at_2000_characters_after_redaction() {
        // `web/agent_report.js`: `printable(redactTaskText(evidence)).slice(-REPORT_EVIDENCE_CHARS)`,
        // and `redactTaskText` itself slices to 2000 characters first, so the
        // fence carries the redacted head of the evidence.
        let report = build_report(&input(None));
        let start = report.find("```text").expect("fence");
        let body = &report[start + 8..];
        let end = body.find("```").expect("closing fence");
        let evidence = &body[..end];
        assert!(evidence.starts_with("line\n"), "redaction keeps the head");
        assert_eq!(evidence.trim_end().chars().count(), 2000);
    }

    #[test]
    fn chinese_headings_follow_the_language_switch() {
        let task = task();
        let mut input = input(Some(&task));
        input.lang = "zh-CN";
        input.generated_at = Some("2026-10-02T00:00:00.000Z");
        let report = build_report(&input);
        assert!(report.starts_with("# Linkr Bee 排查报告"));
        assert!(report.contains("## 助手结论"));
        assert!(report.contains("## 计划"));
        assert!(report.contains("## 串口执行记录"));
        assert!(
            report.ends_with("> 退出码与助手结论都不等于目标达成；请以报告中的证据片段自行核对。")
        );
        assert!(report.contains("命令 · `sha256sum /tmp/app.bin`"));
    }

    #[test]
    fn report_is_capped_and_marked() {
        // Evidence is redaction-capped at 2000 characters per record, so the
        // absolute cap is reached with many records rather than one huge one.
        let records: Vec<ReportRecord> = (0..80)
            .map(|index| ReportRecord {
                id: format!("serial-{index}"),
                payload: "x".repeat(1_900),
                evidence: Some("e".repeat(1_900)),
                ..record()
            })
            .collect();
        let mut input = input(None);
        input.records = &records;
        let report = build_report(&input);
        assert_eq!(
            report.chars().count(),
            REPORT_MAX_CHARS + "\n\n[truncated]".len()
        );
        assert!(report.ends_with("\n\n[truncated]"));
    }

    #[test]
    fn control_bytes_never_break_the_layout() {
        let record = ReportRecord {
            id: "serial-2".into(),
            payload: "printf '\u{1b}[31mred\u{7}'".into(),
            evidence: Some("a\u{0}b\u{1b}[0mc\r\n".into()),
            ..record()
        };
        let mut input = input(None);
        input.records = std::slice::from_ref(&record);
        let report = build_report(&input);
        assert!(!report.contains('\u{1b}'), "escape bytes stripped");
        assert!(!report.contains('\u{7}'), "BEL stripped");
        assert!(!report.contains('\r'), "CR stripped");
    }
}
