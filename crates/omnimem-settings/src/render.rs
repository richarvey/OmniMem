//! The template environment: the web UI's Jinja2 templates on minijinja.

use std::sync::{Arc, RwLock};

use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use chrono::TimeZone as _;
use minijinja::{AutoEscape, Environment, Value};
use tracing::error;

use crate::embedded::TEMPLATES;

/// `"{:,}".format(n)`: digits grouped in threes.
pub(crate) fn thousands(value: &Value) -> String {
    let number = value.to_string();
    let (sign, digits) = number
        .strip_prefix('-')
        .map_or(("", number.as_str()), |d| ("-", d));
    let (whole, fraction) = digits
        .split_once('.')
        .map_or((digits, None), |(w, f)| (w, Some(f)));
    let mut grouped = String::new();
    for (i, c) in whole.chars().enumerate() {
        if i > 0 && (whole.len() - i) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(c);
    }
    match fraction {
        Some(f) => format!("{sign}{grouped}.{f}"),
        None => format!("{sign}{grouped}"),
    }
}

/// A count that has to fit a tile: exact below a thousand, then `1.2k`,
/// `12k`, `123k`, `1.2M`. One decimal only while the figure is under ten
/// of its unit, so a tile never shows more than four characters of number.
fn compact(value: &Value) -> String {
    let Ok(n) = f64::try_from(value.clone()) else {
        return value.to_string();
    };
    if !n.is_finite() || n.abs() < 1000.0 {
        return thousands(value);
    }
    let sign = if n < 0.0 { "-" } else { "" };
    let units = ["", "k", "M", "B", "T"];
    let mut scaled = n.abs();
    let mut unit = 0;
    // 999,500 rounds up to the next unit ("1M"), never to "1000k".
    while scaled >= 999.5 && unit + 1 < units.len() {
        scaled /= 1000.0;
        unit += 1;
    }
    let unit = units[unit];
    let one_decimal = (scaled * 10.0).round() / 10.0;
    if one_decimal < 10.0 && one_decimal.fract() != 0.0 {
        format!("{sign}{one_decimal:.1}{unit}")
    } else {
        format!("{sign}{}{unit}", scaled.round())
    }
}

/// The first `max` characters of a preview, with an ellipsis when that cut
/// something. Lines are kept: `plain` joins them, and needs to see a
/// heading line to join it with a colon.
pub(crate) fn clip(text: &str, max: usize) -> String {
    let mut out: String = text.chars().take(max).collect();
    if text.chars().count() > max {
        out.push('…');
    }
    out
}

/// A one-line preview of markdown: heading marks, emphasis, code ticks and
/// quote marks go, so "# Summary **Who:** x" reads as "Summary Who: x".
fn plain(text: String) -> String {
    let mut out = String::with_capacity(text.len());
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    for (i, line) in lines.iter().enumerate() {
        let heading = line.trim_start().starts_with('#');
        let line = line
            .trim_start()
            .trim_start_matches(['#', '>'])
            .trim_start();
        let line = line
            .strip_prefix("- ")
            .or_else(|| line.strip_prefix("* "))
            .unwrap_or(line);
        if !out.is_empty() {
            out.push(' ');
        }
        out.extend(line.chars().filter(|c| !matches!(c, '*' | '`')));
        // "# Summary" then its text reads "Summary: the text".
        if heading && i + 1 < lines.len() && !out.ends_with([':', '.', '!', '?']) {
            out.push(':');
        }
    }
    out
}

/// HTML escaping as Jinja2's markupsafe does it: `& < > ' "` and nothing
/// else. minijinja also escapes `/`, which browsers read the same way but
/// which would make the panel's output differ from the web UI's byte for byte.
fn markupsafe_escape(text: &str, out: &mut String) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&#34;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
}

/// The panel's Content Security Policy: scripts only from its own files, so
/// a value a template writes can never run. Sent as a header and declared in
/// `base.html` too.
pub(crate) const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; font-src 'self'; connect-src 'self'; form-action 'self'; base-uri 'none'; object-src 'none'; frame-ancestors 'none'";

pub(crate) fn environment(notice: Arc<RwLock<Option<String>>>) -> Environment<'static> {
    let mut env = Environment::new();
    env.set_auto_escape_callback(|_| AutoEscape::Html);
    env.set_formatter(|out, state, value| match value.as_str() {
        Some(text) if state.auto_escape() == AutoEscape::Html && !value.is_safe() => {
            let mut escaped = String::with_capacity(text.len());
            markupsafe_escape(text, &mut escaped);
            out.write_str(&escaped).map_err(minijinja::Error::from)
        }
        _ => minijinja::escape_formatter(out, state, value),
    });
    minijinja_contrib::add_to_environment(&mut env);
    env.set_unknown_method_callback(minijinja_contrib::pycompat::unknown_method_callback);
    env.add_filter("thousands", thousands);
    env.add_filter("compact", compact);
    env.add_filter("plain", plain);
    // For a value going into a query string, before HTML escaping.
    env.add_filter("urlquote", |value: String| crate::pages::quote(&value));
    // For a value that is one path segment (a project name, a memory key):
    // a `/` is encoded too, so it can't add segments.
    env.add_filter("urlsegment", |value: String| {
        crate::pages::quote_segment(&value)
    });
    // An epoch timestamp (seconds, as a string or number) as "7 Nov 2026".
    env.add_filter("epoch_date", |value: Value| {
        let seconds = value
            .as_str()
            .and_then(|s| s.trim().parse::<f64>().ok())
            .or_else(|| f64::try_from(value.clone()).ok())
            .filter(|s| *s > 0.0);
        seconds
            .and_then(|s| chrono::Local.timestamp_opt(s as i64, 0).single())
            .map(|t| t.format("%-d %b %Y").to_string())
            .unwrap_or_default()
    });
    env.add_global("version", env!("CARGO_PKG_VERSION"));
    // Where the panel is served: every link a template writes starts here.
    env.add_global("admin", crate::ADMIN_ROOT);
    // Read at render time, so a note set after start-up reaches every page.
    env.add_function("panel_notice", move || {
        notice
            .read()
            .ok()
            .and_then(|n| n.clone())
            .unwrap_or_default()
    });
    for (name, source) in TEMPLATES {
        env.add_template(name, source)
            .unwrap_or_else(|e| panic!("template {name} doesn't parse: {e:#}"));
    }
    env
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// A plain error page naming what went wrong.
pub(crate) fn failure(message: &str) -> Response {
    error!(error = message, "panel request failed");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Html(format!(
            "<h1>Could not show this page</h1><pre>{}</pre>",
            escape(message)
        )),
    )
        .into_response()
}

/// Render a template, or the error page.
pub(crate) fn page(env: &Environment<'static>, name: &str, context: Value) -> Response {
    // Every page knows where it is, so its links can carry a way back.
    let context = minijinja::context! { here => crate::pages::here(), ..context };
    match env.get_template(name).and_then(|t| t.render(context)) {
        Ok(html) => Html(html).into_response(),
        Err(e) => failure(&format!("rendering {name}: {e:#}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_group_in_threes() {
        assert_eq!(thousands(&Value::from(1_234_567)), "1,234,567");
        assert_eq!(thousands(&Value::from(999)), "999");
        assert_eq!(thousands(&Value::from(-1000)), "-1,000");
        assert_eq!(thousands(&Value::from(12345.5)), "12,345.5");
    }

    #[test]
    fn compact_counts_fit_a_tile() {
        assert_eq!(compact(&Value::from(999)), "999");
        assert_eq!(compact(&Value::from(1000)), "1k");
        assert_eq!(compact(&Value::from(1234)), "1.2k");
        assert_eq!(compact(&Value::from(9949)), "9.9k");
        assert_eq!(compact(&Value::from(9950)), "10k");
        assert_eq!(compact(&Value::from(12_345)), "12k");
        assert_eq!(compact(&Value::from(999_499)), "999k");
        assert_eq!(compact(&Value::from(999_500)), "1M");
        assert_eq!(compact(&Value::from(1_234_567)), "1.2M");
        assert_eq!(compact(&Value::from(-1500)), "-1.5k");
        assert_eq!(compact(&Value::from(3.2)), "3.2");
        assert_eq!(compact(&Value::from("—")), "—");
    }

    #[test]
    fn previews_drop_markdown_marks() {
        assert_eq!(
            plain("# Summary **Who:** the `api` team\n> quoted\n- a point".into()),
            "Summary Who: the api team: quoted a point"
        );
        assert_eq!(
            plain("# Summary\nDocker Sandboxes' kits".into()),
            "Summary: Docker Sandboxes' kits"
        );
        assert_eq!(
            plain("check_contradictions stays".into()),
            "check_contradictions stays"
        );
    }

    #[test]
    fn a_pager_shows_one_ellipsis_per_gap() {
        let env = environment(Arc::default());
        let render = |page: i64, total_pages: i64| {
            env.get_template("partials/pagination.html")
                .unwrap()
                .render(minijinja::context! {
                    page, total_pages, base_url => "/memories", extra_params => "",
                })
                .unwrap()
        };
        let near_start = render(1, 217);
        assert_eq!(near_start.matches("&hellip;").count(), 1, "{near_start}");
        let middle = render(100, 217);
        assert_eq!(middle.matches("&hellip;").count(), 2, "{middle}");
        let short = render(2, 4);
        assert_eq!(short.matches("&hellip;").count(), 0, "{short}");
        // A gap of one page is that page, not an ellipsis standing in for it.
        let one = render(5, 9);
        assert_eq!(one.matches("&hellip;").count(), 0, "{one}");
        assert!(one.contains("?page=7\""), "{one}");
    }

    #[test]
    fn escaping_matches_markupsafe() {
        let mut env = environment(Arc::default());
        env.add_template("t", "{{ v }}").unwrap();
        let rendered = env
            .get_template("t")
            .unwrap()
            .render(minijinja::context! { v => "/a?b=1&c=<\"x\">'y'" })
            .unwrap();
        assert_eq!(rendered, "/a?b=1&amp;c=&lt;&#34;x&#34;&gt;&#39;y&#39;");
    }

    #[test]
    fn every_template_parses() {
        let env = environment(Arc::default());
        assert!(env.get_template("base.html").is_ok());
        assert!(
            env.get_template("partials/token_overhead_content.html")
                .is_ok()
        );
        assert_eq!(env.templates().count(), TEMPLATES.len());
    }

    #[test]
    fn query_values_are_percent_encoded_by_the_filter() {
        let mut env = environment(Arc::default());
        env.add_template("t", "?project={{ v | urlquote }}")
            .unwrap();
        let rendered = env
            .get_template("t")
            .unwrap()
            .render(minijinja::context! { v => "a b&c=<d>" })
            .unwrap();
        assert_eq!(rendered, "?project=a%20b%26c%3D%3Cd%3E");
    }

    /// Where `<script` or an `on...=` attribute starts in `source`, if
    /// anywhere: the two places a template value would run as code, whatever
    /// the HTML escaping did to it. `<script src=` is a file, not code.
    fn inline_script_or_handler(source: &str) -> Option<&str> {
        let lower = source.to_ascii_lowercase();
        for (at, _) in lower.match_indices("<script") {
            let tag = &lower[at..lower[at..].find('>').map_or(lower.len(), |end| at + end)];
            if !tag.contains(" src=") {
                return Some(&source[at..]);
            }
        }
        if let Some(at) = lower.find("javascript:") {
            return Some(&source[at..]);
        }
        // An attribute name starting `on` followed by letters and `=`.
        for (at, _) in lower.match_indices(" on") {
            let rest = &lower[at + 3..];
            let letters = rest.len()
                - rest
                    .trim_start_matches(|c: char| c.is_ascii_lowercase())
                    .len();
            if letters > 0 && rest[letters..].starts_with('=') {
                return Some(&source[at..]);
            }
        }
        None
    }

    /// The panel's Content Security Policy allows no inline script, so a
    /// template value can never be parsed as code. This keeps it that way:
    /// every handler lives in `static/panel.js` and reads data attributes.
    #[test]
    fn no_template_carries_inline_script_or_handlers() {
        for (name, source) in TEMPLATES {
            if let Some(found) = inline_script_or_handler(source) {
                let excerpt: String = found.chars().take(80).collect();
                panic!("{name} has inline script or a handler: {excerpt}");
            }
        }
        assert!(inline_script_or_handler(r#"<form onsubmit="return confirm('x')">"#).is_some());
        assert!(inline_script_or_handler("<SCRIPT>alert(1)</SCRIPT>").is_some());
        assert!(inline_script_or_handler(r#"<a href="javascript:alert(1)">"#).is_some());
        assert!(inline_script_or_handler(r#"<script src="/static/panel.js"></script>"#).is_none());
        assert!(inline_script_or_handler(r#"<button data-confirm="Delete on Monday?">"#).is_none());
    }

    #[test]
    fn the_base_template_sets_the_content_security_policy() {
        let env = environment(Arc::default());
        let html = env
            .get_template("base.html")
            .unwrap()
            .render(minijinja::context! { current_page => "" })
            .unwrap();
        assert!(
            html.contains(r#"http-equiv="Content-Security-Policy""#),
            "{html}"
        );
        assert!(html.contains("script-src 'self';"), "{html}");
        assert!(!html.contains("'unsafe-eval'"), "{html}");
    }
}
