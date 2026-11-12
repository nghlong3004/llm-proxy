//! One-shot localhost callback: validate state before accepting an OAuth code.
use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use std::{collections::HashMap, sync::Arc};
use tokio::sync::{Mutex, oneshot};
type CodeSender = oneshot::Sender<Result<String, String>>;
#[derive(Clone)]
pub(super) struct Callback {
    pub(super) expected: String,
    pub(super) sender: Arc<Mutex<Option<CodeSender>>>,
}
pub(super) async fn oauth_callback(
    State(state): State<Callback>,
    Query(query): Query<HashMap<String, String>>,
) -> impl IntoResponse {
    if query.get("state") != Some(&state.expected) {
        return (
            StatusCode::BAD_REQUEST,
            "Invalid OAuth state. Return to the original sign-in page.",
        );
    }
    let result = match (query.get("code"), query.get("error")) {
        (_, Some(_)) => Err("Consent declined".into()),
        (Some(code), None) if !code.is_empty() && code.len() < 8192 => Ok(code.clone()),
        _ => return (StatusCode::BAD_REQUEST, "Missing authorization code."),
    };
    if let Some(sender) = state.sender.lock().await.take() {
        let _ = sender.send(result);
    }
    (StatusCode::OK, "Sign-in received. Return to your terminal.")
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, body::Body, http::Request, routing::get};
    use tower::ServiceExt;
    #[tokio::test]
    async fn wrong_state_does_not_consume_callback_and_valid_code_is_one_shot() {
        let (tx, mut rx) = oneshot::channel();
        let state = Callback {
            expected: "expected".into(),
            sender: Arc::new(Mutex::new(Some(tx))),
        };
        let app = Router::new()
            .route("/auth/callback", get(oauth_callback))
            .with_state(state);
        let bad = app
            .clone()
            .oneshot(
                Request::get("/auth/callback?state=wrong&code=bad")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(bad.status(), StatusCode::BAD_REQUEST);
        assert!(rx.try_recv().is_err());
        let good = app
            .oneshot(
                Request::get("/auth/callback?state=expected&code=good")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(good.status(), StatusCode::OK);
        assert_eq!(rx.await.unwrap().unwrap(), "good");
    }
    #[tokio::test]
    async fn denied_consent_returns_error_to_login_flow() {
        let (tx, rx) = oneshot::channel();
        let state = Callback {
            expected: "expected".into(),
            sender: Arc::new(Mutex::new(Some(tx))),
        };
        let app = Router::new()
            .route("/auth/callback", get(oauth_callback))
            .with_state(state);
        app.oneshot(
            Request::get("/auth/callback?state=expected&error=access_denied")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
        assert!(rx.await.unwrap().is_err());
    }
}
