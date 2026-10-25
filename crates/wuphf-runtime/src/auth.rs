//! Who authenticates where. Every peer presents a bearer token as
//! `authorization: Bearer <token>`:
//!
//! - a connector session presents its own connector's token, checked against
//!   the connector id in its registration frame ([`ConnectorTokens`]);
//! - media upload and download accept any connector's or provider's token
//!   ([`any_token_auth`]), since providers such as `mediaconv` move media too;
//! - the observer and function services use one shared token each
//!   ([`crate::observer_service::bearer_auth`]); providers are checked by the
//!   function broker itself.

use std::collections::HashMap;
use tonic::metadata::MetadataMap;
use tonic::{Request, Status};
use wuphf_model::ConnectorId;

/// The bearer token a request presents, if any.
pub fn bearer_token(metadata: &MetadataMap) -> Option<&str> {
    metadata
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
}

/// Each configured connector's token.
#[derive(Debug, Default)]
pub struct ConnectorTokens {
    tokens: HashMap<ConnectorId, String>,
}

impl ConnectorTokens {
    pub fn new(tokens: HashMap<ConnectorId, String>) -> Self {
        Self { tokens }
    }

    /// Whether `token` is the one configured for connector `id`.
    pub fn authenticates(&self, id: &ConnectorId, token: Option<&str>) -> bool {
        self.tokens
            .get(id)
            .is_some_and(|expected| token == Some(expected.as_str()))
    }

    /// Every connector's token, for services any connector may use.
    pub fn tokens(&self) -> impl Iterator<Item = &str> {
        self.tokens.values().map(String::as_str)
    }
}

/// An interceptor accepting a request whose bearer token is any of `tokens`.
/// An empty list rejects everything: a working configuration always has at
/// least one connector, so an empty list means a misconfiguration, never
/// "allow all".
// tonic's Interceptor signature fixes the `Err` type to the large `Status`.
#[allow(clippy::result_large_err)]
pub fn any_token_auth(tokens: Vec<String>) -> impl tonic::service::Interceptor + Clone {
    move |request: Request<()>| -> Result<Request<()>, Status> {
        match bearer_token(request.metadata()) {
            Some(presented) if tokens.iter().any(|token| token == presented) => Ok(request),
            _ => Err(Status::unauthenticated("valid bearer token required")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tonic::service::Interceptor;

    fn request(token: Option<&str>) -> Request<()> {
        let mut request = Request::new(());
        if let Some(token) = token {
            request
                .metadata_mut()
                .insert("authorization", format!("Bearer {token}").parse().unwrap());
        }
        request
    }

    #[test]
    fn any_token_auth_with_no_tokens_rejects() {
        let mut interceptor = any_token_auth(Vec::new());
        assert!(interceptor.call(request(None)).is_err());
        assert!(interceptor.call(request(Some(""))).is_err());
        assert!(interceptor.call(request(Some("anything"))).is_err());
    }

    #[test]
    fn any_token_auth_accepts_only_listed_tokens() {
        let mut interceptor = any_token_auth(vec!["a".into(), "b".into()]);
        assert!(interceptor.call(request(Some("b"))).is_ok());
        assert!(interceptor.call(request(Some("c"))).is_err());
        assert!(interceptor.call(request(None)).is_err());
    }

    #[test]
    fn connector_tokens_match_by_connector() {
        let tokens = ConnectorTokens::new(HashMap::from([
            (ConnectorId("wa".into()), "wa-secret".into()),
            (ConnectorId("tg".into()), "tg-secret".into()),
        ]));
        assert!(tokens.authenticates(&ConnectorId("wa".into()), Some("wa-secret")));
        assert!(!tokens.authenticates(&ConnectorId("wa".into()), Some("tg-secret")));
        assert!(!tokens.authenticates(&ConnectorId("wa".into()), None));
        assert!(!tokens.authenticates(&ConnectorId("other".into()), Some("wa-secret")));
    }
}
