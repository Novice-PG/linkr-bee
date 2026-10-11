//! `read_web_page`: the app-side HTTP reader, port of `mobile/src/web-reader.mjs`
//! (spec §2.9). The reader is deliberately plain: no cookies, no credentials,
//! a hard byte cap and a hard timeout, because everything it returns is
//! untrusted evidence that the model will quote back.

use std::time::Duration;

use regex::Regex;
use serde::Serialize;
use serde_json::{json, Value};
use std::sync::OnceLock;

/// `MAX_BYTES = 512 * 1024`.
pub const MAX_PAGE_BYTES: usize = 512 * 1024;
/// `setTimeout(..., 15000)`.
pub const PAGE_TIMEOUT_SECS: u64 = 15;
/// `offset`/`limit` bounds of the schema (spec §2.9).
pub const MAX_PAGE_LIMIT: usize = 16_000;
pub const MAX_FIND_CHARS: usize = 200;
/// At most 40 unique links, text capped at 120 characters.
pub const MAX_LINKS: usize = 40;
pub const MAX_LINK_TEXT: usize = 120;
/// Window starts 200 characters before the match.
pub const FIND_CONTEXT: usize = 200;
/// The exact accept header of the JS reader.
pub const ACCEPT_HEADER: &str = "text/html,text/plain,application/json";

/// Validation failures (exact strings of spec §2.9).
pub const ERR_CREDENTIALS: &str = "Use an HTTP(S) URL without embedded credentials.";
pub const ERR_TIMEOUT: &str = "Web read timed out after 15 seconds.";
pub const ERR_NOT_TEXT: &str =
    "This URL is not a text page. Use the download tool for the destination chosen by the user.";
pub const ERR_TOO_LARGE: &str =
    "Page exceeds the 512 KiB reading limit. Use a smaller document or the target shell.";
pub const ERR_INVALID_ARGS: &str = "Invalid page offset, limit or search text.";

/// One extracted link.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PageLink {
    pub text: String,
    pub url: String,
}

/// The extracted document, before the offset/limit window is applied.
#[derive(Debug, Clone, PartialEq)]
pub struct ExtractedPage {
    pub title: String,
    pub text: String,
    pub links: Vec<PageLink>,
}

/// The tool result: `offset`/`nextOffset` describe the window inside `text`
/// as returned, `totalCharacters` the whole document.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WebPage {
    pub url: String,
    pub title: String,
    pub text: String,
    pub links: Vec<PageLink>,
    pub offset: usize,
    pub next_offset: usize,
    pub total_characters: usize,
    pub has_more: bool,
    /// `null` when no `find` was given, `-1` when it was not found (the JS
    /// reader returns `indexOf`'s sentinel verbatim).
    pub match_index: Option<i64>,
    pub match_found: Option<bool>,
    pub fetched_at: String,
    pub truncated: bool,
    pub untrusted: bool,
}

impl WebPage {
    pub fn to_json(&self) -> Value {
        json!({
            "url": self.url,
            "title": self.title,
            "text": self.text,
            "links": self.links,
            "offset": self.offset,
            "nextOffset": self.next_offset,
            "totalCharacters": self.total_characters,
            "hasMore": self.has_more,
            "matchIndex": self.match_index,
            "matchFound": self.match_found,
            "fetchedAt": self.fetched_at,
            "truncated": self.truncated,
            "untrusted": self.untrusted,
        })
    }
}

/// `webUrl()`: http(s) only, no embedded credentials.
pub fn normalize_url(value: &str) -> Result<String, String> {
    let url = reqwest::Url::parse(value.trim()).map_err(|_| ERR_CREDENTIALS.to_string())?;
    if url.scheme() != "http" && url.scheme() != "https" {
        return Err(ERR_CREDENTIALS.to_string());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(ERR_CREDENTIALS.to_string());
    }
    Ok(url.to_string())
}

/// Resolve a link against the page URL, keeping only plain web links.
pub fn resolve_link(href: &str, base: &str) -> Option<String> {
    let base = reqwest::Url::parse(base).ok()?;
    let joined = base.join(href.trim()).ok()?;
    if joined.scheme() != "http" && joined.scheme() != "https" {
        return None;
    }
    if !joined.username().is_empty() || joined.password().is_some() {
        return None;
    }
    Some(joined.to_string())
}

/// The argument check that runs before any network traffic.
pub fn validate_page_args(offset: i64, limit: i64, find: Option<&str>) -> Result<(), String> {
    if offset < 0
        || limit < 1
        || limit > MAX_PAGE_LIMIT as i64
        || offset > i32::MAX as i64
        || match find {
            None => false,
            Some(text) => {
                let chars = text.chars().count();
                text.trim().is_empty() || chars > MAX_FIND_CHARS
            }
        }
    {
        return Err(ERR_INVALID_ARGS.to_string());
    }
    Ok(())
}

/// `[\t ]+` → " ", blank runs → "\n\n", trim.
pub fn normalize_text(source: &str) -> String {
    static SPACES: OnceLock<Regex> = OnceLock::new();
    static BLANKS: OnceLock<Regex> = OnceLock::new();
    let spaces = SPACES.get_or_init(|| Regex::new(r"[\t ]+").expect("spaces"));
    let blanks = BLANKS.get_or_init(|| Regex::new(r"\n\s*\n").expect("blanks"));
    let collapsed = spaces.replace_all(source, " ");
    let collapsed = blanks.replace_all(&collapsed, "\n\n");
    collapsed.trim().to_string()
}

fn strip_tags(html: &str) -> String {
    static TAGS: OnceLock<Regex> = OnceLock::new();
    let tags = TAGS.get_or_init(|| Regex::new(r"(?s)<[^>]*>").expect("tags"));
    decode_entities(&tags.replace_all(html, ""))
}

/// The handful of entities a text-only reader must understand to keep the
/// extracted text readable; everything else passes through untouched.
fn decode_entities(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
}

/// `extractWebPage()`. Without a DOM the extraction is regex-based, which is
/// enough for the bounded evidence the tool returns: strip the inert elements,
/// collect the first 40 links, take `main`/`article`/`body`.
pub fn extract_page(source: &str, url: &str, content_type: &str) -> ExtractedPage {
    if !content_type.contains("html") {
        return ExtractedPage {
            title: String::new(),
            text: source.to_string(),
            links: Vec::new(),
        };
    }
    let mut document = source.to_string();
    for tag in ["script", "style", "noscript", "iframe", "template", "svg"] {
        let re =
            Regex::new(&format!(r"(?is)<{tag}\b[^>]*>.*?</{tag}\s*>")).expect("element pattern");
        document = re.replace_all(&document, "").to_string();
    }

    let mut links: Vec<PageLink> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    let anchor = Regex::new(
        r#"(?is)<a\b[^>]*href\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s>]+))[^>]*>(.*?)</a\s*>"#,
    )
    .expect("anchor pattern");
    for captures in anchor.captures_iter(&document) {
        if links.len() >= MAX_LINKS {
            break;
        }
        let href = captures
            .get(1)
            .or_else(|| captures.get(2))
            .or_else(|| captures.get(3))
            .map(|m| m.as_str())
            .unwrap_or("");
        let Some(resolved) = resolve_link(href, url) else {
            continue;
        };
        if seen.contains(&resolved) {
            continue;
        }
        let mut text = normalize_text(&strip_tags(&captures[4]));
        let truncated: String = text.chars().take(MAX_LINK_TEXT).collect();
        text = truncated;
        seen.push(resolved.clone());
        links.push(PageLink {
            text,
            url: resolved,
        });
    }

    // Block boundaries become newlines before the tags disappear — the same
    // node list as `web-reader.mjs` appends to (`main`/`article` are NOT in it,
    // they must survive so the block picker below can find them).
    let boundary = Regex::new(r"(?i)</?(?:br|p|div|li|pre|h[1-4]|tr|section)\b[^>]*>")
        .expect("boundary pattern");
    document = boundary.replace_all(&document, "\n").to_string();

    let title = Regex::new(r"(?is)<title[^>]*>(.*?)</title>")
        .ok()
        .and_then(|re| re.captures(&document))
        .map(|caps| normalize_text(&strip_tags(&caps[1])))
        .unwrap_or_default();

    let body = first_block(&document, "main")
        .or_else(|| first_block(&document, "article"))
        .or_else(|| first_block(&document, "body"))
        .unwrap_or(&document);
    let text = normalize_text(&strip_tags(body));

    ExtractedPage { title, text, links }
}

fn first_block<'a>(document: &'a str, tag: &str) -> Option<&'a str> {
    let re = Regex::new(&format!(r"(?is)<{tag}\b[^>]*>(.*?)</{tag}\s*>")).expect("block pattern");
    re.captures(document)
        .map(|caps| caps.get(1).map(|m| m.as_str()).unwrap_or(""))
        .filter(|inner| !inner.trim().is_empty())
}

/// The pure part of the reader: `offset`/`limit`/`find` → the returned window
/// (`readWebPage`'s arithmetic). `find` starts at `offset`, exactly like
/// `indexOf(find, offset)` does in JS — but the returned match index is a
/// character index into `text` rather than into the case-folded copy, because
/// `to_lowercase` is not one character out for one character in (`İ` expands).
pub fn window(
    text: &str,
    offset: usize,
    limit: usize,
    find: Option<&str>,
) -> (usize, usize, Option<i64>, Option<bool>) {
    let chars: Vec<char> = text.chars().collect();
    let total = chars.len();
    let index_from = |needle: &str, from: usize| -> Option<usize> {
        let from = from.min(total);
        // Fold for a case-insensitive match, but carry each folded character's
        // position in the original char vector alongside it: a raw index into
        // the folded vector would drift from `text`'s own indices whenever a
        // character lowers to more than one (the same fix as `tool_search_log`).
        let mut hay: Vec<char> = Vec::new();
        let mut origin: Vec<usize> = Vec::new();
        for (index, ch) in chars[from..].iter().enumerate() {
            for folded in ch.to_lowercase() {
                hay.push(folded);
                origin.push(from + index);
            }
        }
        let needle: Vec<char> = needle.chars().flat_map(|c| c.to_lowercase()).collect();
        if needle.is_empty() || hay.len() < needle.len() {
            return None;
        }
        hay.windows(needle.len())
            .position(|w| w == needle.as_slice())
            .map(|found| origin[found])
    };
    let match_index = find.map(|needle| match index_from(needle, offset) {
        Some(found) => found as i64,
        None => -1,
    });
    let start = match (find, match_index) {
        (None, _) => total.min(offset),
        (Some(_), Some(index)) if index >= 0 => {
            total.min(offset.max((index as usize).saturating_sub(FIND_CONTEXT)))
        }
        // Not found: the window is empty at the end of the document.
        _ => total,
    };
    let next = total.min(start + limit);
    let match_found = find.map(|_| match_index.unwrap_or(-1) >= 0);
    (start, next, match_index, match_found)
}

/// Assemble the tool result from an already extracted page.
pub fn build_page(
    final_url: String,
    page: ExtractedPage,
    offset: usize,
    limit: usize,
    find: Option<&str>,
    fetched_at: String,
) -> WebPage {
    let text = normalize_text(&page.text);
    let total = text.chars().count();
    let (start, next, match_index, match_found) = window(&text, offset, limit, find);
    let body: String = text
        .chars()
        .skip(start)
        .take(next.saturating_sub(start))
        .collect();
    WebPage {
        url: final_url,
        title: page.title,
        text: body,
        links: page.links,
        offset: start,
        next_offset: next,
        total_characters: total,
        has_more: next < total,
        match_index,
        match_found,
        fetched_at,
        truncated: start > 0 || next < total,
        untrusted: true,
    }
}

/// One client for every page read, so two fetches to the same host reuse the
/// connection instead of re-establishing it. Built exactly as before — no
/// connect timeout of its own; the per-request timeout below still bounds the
/// whole call.
fn page_client() -> Result<&'static reqwest::Client, String> {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    if let Some(client) = CLIENT.get() {
        return Ok(client);
    }
    let built = reqwest::Client::builder()
        .build()
        .map_err(|e| e.to_string())?;
    Ok(CLIENT.get_or_init(|| built))
}

fn wrap(url: &str, error: String) -> String {
    format!(
        "Unable to read {url}: {error}. Browser CORS or network policy may block access. If a target shell is available, consider curl/wget under the current execution mode."
    )
}

fn is_text_content_type(content_type: &str) -> bool {
    static TEXT: OnceLock<Regex> = OnceLock::new();
    let text =
        TEXT.get_or_init(|| Regex::new(r"^(text/|application/(json|xhtml\+xml))").expect("ct"));
    text.is_match(content_type)
}

/// The reader itself: one bounded GET, then extraction and the window.
pub async fn read_web_page(
    url: &str,
    offset: i64,
    limit: i64,
    find: Option<&str>,
) -> Result<WebPage, String> {
    let url = normalize_url(url)?;
    validate_page_args(offset, limit, find)?;
    let limit = limit as usize;
    let offset = offset as usize;
    let client = page_client().map_err(|error| wrap(&url, error))?;
    let request = client
        .get(&url)
        .header(reqwest::header::ACCEPT, ACCEPT_HEADER)
        // No cookies, no referrer: the reader carries no user identity.
        .header(reqwest::header::REFERER, "")
        .timeout(Duration::from_secs(PAGE_TIMEOUT_SECS));

    let response =
        match tokio::time::timeout(Duration::from_secs(PAGE_TIMEOUT_SECS), request.send()).await {
            Err(_) => return Err(ERR_TIMEOUT.to_string()),
            Ok(Err(error)) => return Err(wrap(&url, error.to_string())),
            Ok(Ok(response)) => response,
        };
    if !response.status().is_success() {
        let status = response.status().as_u16();
        return Err(wrap(&url, format!("HTTP {status}")));
    }
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .map(|value| value.to_str().unwrap_or("").to_string())
        .unwrap_or_default();
    if !is_text_content_type(&content_type) {
        return Err(wrap(&url, ERR_NOT_TEXT.to_string()));
    }
    let final_url = response.url().to_string();
    let final_url = normalize_url(&final_url).unwrap_or_else(|_| url.clone());

    let mut source = String::new();
    let mut decoder = crate::journal::Utf8Decoder::default();
    let mut stream = response.bytes_stream();
    use futures::StreamExt;
    let mut budget = MAX_PAGE_BYTES;
    loop {
        let chunk =
            match tokio::time::timeout(Duration::from_secs(PAGE_TIMEOUT_SECS), stream.next()).await
            {
                Err(_) => return Err(ERR_TIMEOUT.to_string()),
                Ok(None) => break,
                Ok(Some(Ok(chunk))) => chunk,
                Ok(Some(Err(error))) => return Err(wrap(&url, error.to_string())),
            };
        budget = budget.saturating_sub(chunk.len());
        if budget == 0 {
            return Err(wrap(&url, ERR_TOO_LARGE.to_string()));
        }
        source.push_str(&decoder.decode(&chunk));
    }
    source.push_str(&decoder.decode(&[]));

    let page = extract_page(&source, &final_url, &content_type);
    let fetched_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    Ok(build_page(final_url, page, offset, limit, find, fetched_at))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_rules_match_the_js_reader() {
        assert_eq!(
            normalize_url("ftp://example.com/x").unwrap_err(),
            ERR_CREDENTIALS
        );
        assert_eq!(
            normalize_url("https://user:pass@example.com/").unwrap_err(),
            ERR_CREDENTIALS
        );
        assert_eq!(normalize_url("not a url").unwrap_err(), ERR_CREDENTIALS);
        assert_eq!(
            normalize_url("https://example.com/docs").unwrap(),
            "https://example.com/docs"
        );
        assert_eq!(
            resolve_link("../api", "https://example.com/docs/index.html").unwrap(),
            "https://example.com/api"
        );
        assert_eq!(resolve_link("mailto:a@b.c", "https://example.com/"), None);
        assert!(resolve_link("//other/x", "https://example.com/").is_some());
    }

    #[test]
    fn argument_validation_uses_the_exact_message() {
        assert_eq!(
            validate_page_args(-1, 100, None).unwrap_err(),
            ERR_INVALID_ARGS
        );
        assert_eq!(
            validate_page_args(0, 0, None).unwrap_err(),
            ERR_INVALID_ARGS
        );
        assert_eq!(
            validate_page_args(0, 16001, None).unwrap_err(),
            ERR_INVALID_ARGS
        );
        assert_eq!(
            validate_page_args(0, 100, Some("   ")).unwrap_err(),
            ERR_INVALID_ARGS
        );
        assert_eq!(
            validate_page_args(0, 100, Some(&"x".repeat(201))).unwrap_err(),
            ERR_INVALID_ARGS
        );
        assert!(validate_page_args(0, 16_000, Some("kernel panic")).is_ok());
    }

    #[test]
    fn window_follows_the_js_arithmetic() {
        let text: String = (0..500)
            .map(|i| if i % 50 == 0 { '\n' } else { 'a' })
            .collect();
        // Plain offset window.
        let (start, next, index, found) = window(&text, 10, 20, None);
        assert_eq!((start, next, index, found), (10, 30, None, None));
        // find opens the window 200 characters before the match; the search
        // itself starts at `offset`, like indexOf(find, offset).
        let (start, next, index, found) = window(&text, 0, 50, Some("aaa"));
        assert_eq!(index, Some(1));
        assert_eq!(start, 0); // saturating_sub at the head
        assert_eq!(found, Some(true));
        assert_eq!(next, 50);
        // A match later in the text moves the window.
        let haystack = format!("{}TARGET{}", "x".repeat(300), "y".repeat(100));
        let (start, next, index, found) = window(&haystack, 0, 40, Some("TARGET"));
        assert_eq!(index, Some(300));
        assert_eq!(start, 100);
        assert_eq!(next, 140);
        assert_eq!(found, Some(true));
        // Searching from an offset past the match misses it (-1), like JS.
        let (_, _, index, found) = window(&haystack, 400, 40, Some("TARGET"));
        assert_eq!(index, Some(-1));
        assert_eq!(found, Some(false));
        // No match: the window is empty, hasMore false, matchFound false.
        let (start, next, index, found) = window("hello world", 0, 5, Some("absent"));
        assert_eq!((start, next, index, found), (11, 11, Some(-1), Some(false)));
        // Case-insensitive.
        assert_eq!(window("Hello", 0, 5, Some("hELLO")).3, Some(true));
        // No find at all: matchIndex and matchFound are null.
        assert_eq!(window("Hello", 0, 5, None), (0, 5, None, None));
    }

    #[test]
    fn window_find_index_does_not_drift_on_expanding_case_fold() {
        // `İ` (U+0130) lowercases to two characters, so a folded index used
        // directly would point one past the real match.
        let text = format!("{}İTARGET", "a".repeat(10));
        let (start, next, index, found) = window(&text, 0, 40, Some("target"));
        assert_eq!(index, Some(11), "the match index is the text's own index");
        assert_eq!(found, Some(true));
        assert_eq!((start, next), (0, 17));
    }

    #[test]
    fn window_is_truncated_flag_and_has_more() {
        let text = "a".repeat(100);
        let page = build_page(
            "https://example.com/".into(),
            extract_page(&text, "https://example.com/", "text/plain"),
            0,
            16_000,
            None,
            "2026-01-01T00:00:00Z".into(),
        );
        assert!(!page.truncated);
        assert!(!page.has_more);
        assert_eq!(page.text.len(), 100);
        assert_eq!(page.total_characters, 100);
        assert!(page.untrusted);

        let page = build_page(
            "https://example.com/".into(),
            extract_page(&text, "https://example.com/", "text/plain"),
            40,
            16_000,
            None,
            "2026-01-01T00:00:00Z".into(),
        );
        assert!(page.truncated);
        assert!(!page.has_more);
        assert_eq!(page.offset, 40);
        assert_eq!(page.text.len(), 60);
    }

    #[test]
    fn extraction_strips_inert_elements_and_collects_links() {
        let html = r#"<!doctype html><html><head><title> Linkr Docs </title>
            <style>body{color:red}</style><script>alert(1)</script></head>
            <body><nav><a href="/a">Alpha</a></nav>
            <main><h1>Heading</h1><p>First&nbsp;para.</p>
            <a href="/b">Beta docs</a> <a href="/a">Alpha again</a>
            <a href="mailto:x@y.z">mail</a>
            <iframe src="x"></iframe><svg><g/></svg>
            <p>Second para.</p></main></body></html>"#;
        let page = extract_page(html, "https://example.com/docs", "text/html; charset=utf-8");
        assert_eq!(page.title, "Linkr Docs");
        assert!(!page.text.contains("alert(1)"));
        assert!(!page.text.contains("color:red"));
        assert!(!page.text.contains("iframe"));
        assert!(page.text.contains("First para."));
        assert!(page.text.contains("Second para."));
        // Duplicate hrefs collapse, non-web links are skipped.
        assert_eq!(page.links.len(), 2);
        assert_eq!(page.links[0].url, "https://example.com/a");
        assert_eq!(page.links[0].text, "Alpha");
        assert_eq!(page.links[1].url, "https://example.com/b");
        // `main`/`article` wins over the nav: the text starts at the heading.
        // The anchors themselves stay in the text; only the link list dedupes.
        assert!(page.text.starts_with("Heading"), "main block wins");
        assert!(page.text.contains("Alpha again"));
    }

    #[test]
    fn non_html_content_type_returns_the_raw_body() {
        let page = extract_page("plain body", "https://example.com/x", "text/plain");
        assert_eq!(page.title, "");
        assert_eq!(page.text, "plain body");
        assert!(page.links.is_empty());
        let json = extract_page("{\"a\":1}", "https://example.com/x", "application/json");
        assert_eq!(json.text, "{\"a\":1}");
    }

    #[test]
    fn content_type_gate_matches_the_js_regex() {
        assert!(is_text_content_type("text/html; charset=utf-8"));
        assert!(is_text_content_type("text/plain"));
        assert!(is_text_content_type("application/json"));
        assert!(is_text_content_type("application/xhtml+xml"));
        assert!(!is_text_content_type("application/octet-stream"));
        assert!(!is_text_content_type("image/png"));
        assert!(!is_text_content_type(""));
        assert!(!is_text_content_type("application/pdf"));
    }

    #[test]
    fn error_strings_are_verbatim() {
        assert_eq!(
            ERR_NOT_TEXT,
            "This URL is not a text page. Use the download tool for the destination chosen by the user."
        );
        assert_eq!(
            ERR_TOO_LARGE,
            "Page exceeds the 512 KiB reading limit. Use a smaller document or the target shell."
        );
        assert_eq!(ERR_TIMEOUT, "Web read timed out after 15 seconds.");
        assert_eq!(
            ERR_INVALID_ARGS,
            "Invalid page offset, limit or search text."
        );
        let wrapped = wrap("https://example.com/", "HTTP 404".to_string());
        assert_eq!(
            wrapped,
            "Unable to read https://example.com/: HTTP 404. Browser CORS or network policy may block access. If a target shell is available, consider curl/wget under the current execution mode."
        );
    }

    #[test]
    fn normalize_text_collapses_blank_runs() {
        // `web-reader.mjs`: `replace(/[\t ]+/g, " ").replace(/\n\s*\n/g, "\n\n").trim()`.
        assert_eq!(normalize_text("a \t b"), "a b");
        assert_eq!(normalize_text("a\n\n\n\nb"), "a\n\nb");
        assert_eq!(normalize_text("  padded\n"), "padded");
    }

    /// Contract test: the reader constants and messages come from the spec
    /// section that quotes `web-reader.mjs`.
    #[test]
    fn spec_section_2_9_documents_every_literal() {
        const SPEC: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/specs/AGENT_SPEC.md");
        let spec = std::fs::read_to_string(SPEC).expect("AGENT_SPEC.md");
        for literal in [
            ERR_CREDENTIALS,
            ERR_TIMEOUT,
            ERR_NOT_TEXT,
            ERR_TOO_LARGE,
            ERR_INVALID_ARGS,
            ACCEPT_HEADER,
        ] {
            assert!(spec.contains(literal), "spec lost literal {literal:?}");
        }
        assert!(spec.contains("512 KiB"));
        assert!(spec.contains("matchIndex"));
    }

    /// The exact failure wrapper and the byte cap are what `web-reader.mjs`
    /// emits; the JS file itself is not shipped in this tree, so the spec is
    /// the contract source here.
    #[test]
    fn budgets_follow_the_spec() {
        assert_eq!(MAX_PAGE_BYTES, 512 * 1024);
        assert_eq!(PAGE_TIMEOUT_SECS, 15);
        assert_eq!(MAX_PAGE_LIMIT, 16_000);
        assert_eq!(MAX_LINKS, 40);
        assert_eq!(MAX_LINK_TEXT, 120);
        assert_eq!(FIND_CONTEXT, 200);
    }
}
