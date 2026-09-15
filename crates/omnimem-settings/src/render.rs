//! The template environment: the web UI's Jinja2 templates on minijinja.

use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use minijinja::{AutoEscape, Environment, Value};
use tracing::error;

use crate::embedded::TEMPLATES;

/// `"{:,}".format(n)`: digits grouped in threes.
fn thousands(value: Value) -> String {
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

pub(crate) fn environment() -> Environment<'static> {
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
    env.add_global("version", env!("CARGO_PKG_VERSION"));
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
        assert_eq!(thousands(Value::from(1234567)), "1,234,567");
        assert_eq!(thousands(Value::from(999)), "999");
        assert_eq!(thousands(Value::from(-1000)), "-1,000");
        assert_eq!(thousands(Value::from(12345.5)), "12,345.5");
    }

    #[test]
    fn escaping_matches_markupsafe() {
        let mut env = environment();
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
        let env = environment();
        assert!(env.get_template("base.html").is_ok());
        assert!(
            env.get_template("partials/token_overhead_content.html")
                .is_ok()
        );
        assert_eq!(env.templates().count(), TEMPLATES.len());
    }
}
