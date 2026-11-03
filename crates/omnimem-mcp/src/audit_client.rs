//! Who a tool call came from, for the audit log: the name the client gave
//! when it initialised, and the OAuth client id when it signed in with OAuth.

use rmcp::RoleServer;
use rmcp::service::RequestContext;

/// The OAuth client a request's access token was issued to. The
/// authentication layer adds it to the request, and rmcp hands the request's
/// extensions on to the handler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OAuthClient(pub String);

/// The client's name, then its OAuth client id in brackets when there is one.
/// Empty if the client gave neither.
pub(crate) fn label(name: Option<&str>, oauth_id: Option<&str>) -> String {
    let name = name.map(str::trim).filter(|n| !n.is_empty());
    let oauth_id = oauth_id.map(str::trim).filter(|id| !id.is_empty());
    match (name, oauth_id) {
        (Some(name), Some(id)) => format!("{name} ({id})"),
        (Some(name), None) => name.to_owned(),
        (None, Some(id)) => id.to_owned(),
        (None, None) => String::new(),
    }
}

pub(crate) fn describe(context: &RequestContext<RoleServer>) -> String {
    let name = context.client_info().map(|info| info.name);
    let oauth = context
        .extensions
        .get::<axum::http::request::Parts>()
        .and_then(|parts| parts.extensions.get::<OAuthClient>());
    label(name.as_deref(), oauth.map(|c| c.0.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_label_names_the_client_and_its_oauth_id() {
        assert_eq!(label(Some("claude-code"), None), "claude-code");
        assert_eq!(
            label(Some(" claude-ai "), Some("abc-123")),
            "claude-ai (abc-123)"
        );
        assert_eq!(label(None, Some("abc-123")), "abc-123");
        assert_eq!(label(Some("  "), None), "");
        assert_eq!(label(None, None), "");
    }
}
