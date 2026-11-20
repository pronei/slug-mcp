//! Relay to the transit agent (Wayfinder): passes the rider's words through and
//! returns the agent's answer. The agent decides what a request means and what
//! to write; nothing here interprets it.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Result, anyhow};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::util::InvalidInput;

pub const URL_ENV: &str = "SLUG_MCP_TRANSIT_AGENT_URL";
pub const TOKEN_ENV: &str = "SLUG_MCP_TRANSIT_AGENT_TOKEN";

const MAX_REQUEST_CHARS: usize = 1000;
// The agent may plan routes and call an LLM before it answers.
const TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, Clone, Copy, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProposalAnswer {
    /// Apply the proposed plan.
    Accept,
    /// Decline it and keep the current plan.
    KeepCurrent,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct UpdateAgendaRequest {
    /// What the user wants changed or answered about today's agenda, in their own words (e.g. "add a stop at the pharmacy before dinner", "push groceries to after 3", "can I still fit the haircut?"). Pass it through as said; the transit agent interprets it. Omit only when answering a proposal.
    pub request: Option<String>,
    /// The user's answer to a proposal an earlier call returned. Send together with `proposal_id`, and without `request`.
    pub answer: Option<ProposalAnswer>,
    /// The proposal ID from the earlier result that asked for an answer.
    pub proposal_id: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Payload {
    Request {
        text: String,
    },
    #[serde(rename_all = "camelCase")]
    Answer {
        proposal_id: String,
        answer: ProposalAnswer,
    },
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Envelope {
    request_id: String,
    sent_at: u64,
    #[serde(flatten)]
    payload: Payload,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AgentResponse {
    status: String,
    #[serde(default)]
    reply: String,
    proposal_id: Option<String>,
}

pub struct AgendaService {
    http: reqwest::Client,
    endpoint: Option<String>,
    token: Option<String>,
    counter: AtomicU64,
}

impl AgendaService {
    pub fn new(http: reqwest::Client, endpoint: Option<String>, token: Option<String>) -> Self {
        Self {
            http,
            endpoint: endpoint.filter(|s| !s.trim().is_empty()),
            token: token.filter(|s| !s.trim().is_empty()),
            counter: AtomicU64::new(0),
        }
    }

    pub async fn update(&self, req: UpdateAgendaRequest) -> Result<String> {
        let payload = build_payload(req)?;

        let Some(endpoint) = &self.endpoint else {
            return Ok(format!(
                "Agenda updates are not configured on this server, so nothing was changed.\n\
                 Set `{URL_ENV}` to the transit agent's request endpoint \
                 (and `{TOKEN_ENV}` if it requires a token)."
            ));
        };

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let envelope = Envelope {
            request_id: format!(
                "slug-{:x}-{:x}",
                now.as_nanos(),
                self.counter.fetch_add(1, Ordering::Relaxed)
            ),
            sent_at: now.as_millis() as u64,
            payload,
        };

        let mut call = self.http.post(endpoint).timeout(TIMEOUT).json(&envelope);
        if let Some(token) = &self.token {
            call = call.bearer_auth(token);
        }

        // The endpoint is often a private tunnel address; keep it out of tool output.
        let resp = call.send().await.map_err(|e| {
            anyhow!(
                "Could not reach the transit agent ({}). The agenda may not have changed.",
                e.without_url()
            )
        })?;
        let status = resp.status();
        if !status.is_success() {
            return Err(anyhow!(
                "The transit agent answered HTTP {status}. The agenda was not confirmed as changed."
            ));
        }
        let body: AgentResponse = resp.json().await.map_err(|e| {
            anyhow!(
                "The transit agent's answer could not be read ({}). Check the agenda before retrying.",
                e.without_url()
            )
        })?;

        Ok(format_response(&body))
    }
}

fn build_payload(req: UpdateAgendaRequest) -> Result<Payload> {
    let text = req
        .request
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty());
    let proposal_id = req
        .proposal_id
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty());

    match (text, req.answer, proposal_id) {
        (Some(text), None, None) => {
            if text.chars().count() > MAX_REQUEST_CHARS {
                return Err(InvalidInput::err(format!(
                    "`request` is too long; keep it under {MAX_REQUEST_CHARS} characters."
                )));
            }
            Ok(Payload::Request { text })
        }
        (None, Some(answer), Some(proposal_id)) => Ok(Payload::Answer {
            proposal_id,
            answer,
        }),
        (None, None, None) => Err(InvalidInput::err(
            "Pass `request` with what the user wants, or `answer` with `proposal_id` to answer a proposal.",
        )),
        (Some(_), _, _) => Err(InvalidInput::err(
            "Pass either `request`, or `answer` with `proposal_id`, not both.",
        )),
        (None, _, _) => Err(InvalidInput::err(
            "`answer` and `proposal_id` go together: the answer (accept or keep_current) and the proposal ID from the earlier result.",
        )),
    }
}

fn format_response(body: &AgentResponse) -> String {
    let reply = body.reply.trim();
    match body.status.as_str() {
        "needs_answer" => {
            let how = match &body.proposal_id {
                Some(id) => format!(
                    "Nothing has changed yet. Ask the user, then call `update_agenda` again with \
                     proposal_id \"{id}\" and answer \"accept\" or \"keep_current\"."
                ),
                None => "Nothing has changed yet. Ask the user, then call `update_agenda` again \
                         with their answer as `request`."
                    .to_string(),
            };
            format!("{reply}\n\n{how}")
        }
        "rejected" => format!("Not done: {reply}"),
        "done" => reply.to_string(),
        other => format!("{reply}\n\n(transit agent status: {other})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    use axum::{Json, Router, http::HeaderMap, http::StatusCode, routing::post};
    use serde_json::{Value, json};

    type Seen = Arc<Mutex<Vec<(Option<String>, Value)>>>;

    async fn fake_agent(status: StatusCode, answer: Value) -> (String, Seen) {
        let seen: Seen = Arc::default();
        let recorder = seen.clone();
        let app = Router::new().route(
            "/api/agent/requests",
            post(move |headers: HeaderMap, Json(body): Json<Value>| {
                let recorder = recorder.clone();
                let answer = answer.clone();
                async move {
                    let auth = headers
                        .get("authorization")
                        .map(|v| v.to_str().unwrap().to_string());
                    recorder.lock().unwrap().push((auth, body));
                    (status, Json(answer))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "http://{}/api/agent/requests",
            listener.local_addr().unwrap()
        );
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (url, seen)
    }

    fn service(url: Option<String>, token: Option<&str>) -> AgendaService {
        AgendaService::new(reqwest::Client::new(), url, token.map(str::to_string))
    }

    fn request(text: &str) -> UpdateAgendaRequest {
        UpdateAgendaRequest {
            request: Some(text.to_string()),
            answer: None,
            proposal_id: None,
        }
    }

    #[tokio::test]
    async fn without_an_endpoint_it_says_which_variable_to_set() {
        let out = service(None, None)
            .update(request("add the pharmacy"))
            .await
            .unwrap();
        assert!(out.contains(URL_ENV), "{out}");
        assert!(out.contains("nothing was changed"), "{out}");
    }

    #[tokio::test]
    async fn a_request_is_passed_through_and_the_reply_returned() {
        let (url, seen) = fake_agent(
            StatusCode::OK,
            json!({ "status": "done", "reply": "Pharmacy added after lunch. Leave by 1:10." }),
        )
        .await;
        let out = service(Some(url), Some("s3cret"))
            .update(request("  add the pharmacy before dinner "))
            .await
            .unwrap();
        assert_eq!(out, "Pharmacy added after lunch. Leave by 1:10.");

        let seen = seen.lock().unwrap();
        let (auth, body) = &seen[0];
        assert_eq!(auth.as_deref(), Some("Bearer s3cret"));
        assert_eq!(body["kind"], "request");
        assert_eq!(body["text"], "add the pharmacy before dinner");
        assert!(body["requestId"].as_str().unwrap().starts_with("slug-"));
        assert!(body["sentAt"].as_u64().unwrap() > 1_700_000_000_000);
    }

    #[tokio::test]
    async fn no_token_means_no_authorization_header() {
        let (url, seen) =
            fake_agent(StatusCode::OK, json!({ "status": "done", "reply": "ok" })).await;
        service(Some(url), None).update(request("x")).await.unwrap();
        assert_eq!(seen.lock().unwrap()[0].0, None);
    }

    #[tokio::test]
    async fn each_call_gets_its_own_request_id() {
        let (url, seen) =
            fake_agent(StatusCode::OK, json!({ "status": "done", "reply": "ok" })).await;
        let svc = service(Some(url), None);
        svc.update(request("x")).await.unwrap();
        svc.update(request("x")).await.unwrap();
        let seen = seen.lock().unwrap();
        assert_ne!(seen[0].1["requestId"], seen[1].1["requestId"]);
    }

    #[tokio::test]
    async fn a_proposal_says_nothing_changed_and_how_to_answer() {
        let (url, _) = fake_agent(
            StatusCode::OK,
            json!({
                "status": "needs_answer",
                "reply": "I can add the pharmacy, but groceries move to 4:10. OK?",
                "proposalId": "n-7"
            }),
        )
        .await;
        let out = service(Some(url), None)
            .update(request("add the pharmacy"))
            .await
            .unwrap();
        assert!(out.starts_with("I can add the pharmacy"), "{out}");
        assert!(out.contains("Nothing has changed yet"), "{out}");
        assert!(out.contains("\"n-7\""), "{out}");
        assert!(out.contains("keep_current"), "{out}");
    }

    #[tokio::test]
    async fn an_answer_is_sent_with_its_proposal_id() {
        let (url, seen) = fake_agent(
            StatusCode::OK,
            json!({ "status": "done", "reply": "Plan updated." }),
        )
        .await;
        let out = service(Some(url), None)
            .update(UpdateAgendaRequest {
                request: None,
                answer: Some(ProposalAnswer::KeepCurrent),
                proposal_id: Some("n-7".into()),
            })
            .await
            .unwrap();
        assert_eq!(out, "Plan updated.");
        let body = &seen.lock().unwrap()[0].1;
        assert_eq!(body["kind"], "answer");
        assert_eq!(body["proposalId"], "n-7");
        assert_eq!(body["answer"], "keep_current");
        assert!(body.get("text").is_none());
    }

    #[tokio::test]
    async fn a_rejection_is_reported_as_not_done() {
        let (url, _) = fake_agent(
            StatusCode::OK,
            json!({ "status": "rejected", "reply": "The pharmacy closes at 6." }),
        )
        .await;
        let out = service(Some(url), None)
            .update(request("pharmacy at 9pm"))
            .await
            .unwrap();
        assert_eq!(out, "Not done: The pharmacy closes at 6.");
    }

    #[tokio::test]
    async fn a_failed_call_is_an_error_that_hides_the_endpoint() {
        let (url, _) = fake_agent(StatusCode::INTERNAL_SERVER_ERROR, json!({})).await;
        let err = service(Some(url.clone()), None)
            .update(request("x"))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("500"), "{err}");
        assert!(err.contains("not confirmed"), "{err}");

        let err = service(Some("http://127.0.0.1:1/api/agent/requests".into()), None)
            .update(request("x"))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("Could not reach"), "{err}");
        assert!(!err.contains("127.0.0.1"), "{err}");
    }

    #[tokio::test]
    async fn bad_argument_combinations_are_caller_errors_and_send_nothing() {
        let (url, seen) =
            fake_agent(StatusCode::OK, json!({ "status": "done", "reply": "ok" })).await;
        let svc = service(Some(url), None);
        let cases = [
            (None, None, None),
            (Some("   "), None, None),
            (Some("x"), Some(ProposalAnswer::Accept), Some("n-1")),
            (None, Some(ProposalAnswer::Accept), None),
            (None, None, Some("n-1")),
        ];
        for (request, answer, proposal_id) in cases {
            let err = svc
                .update(UpdateAgendaRequest {
                    request: request.map(str::to_string),
                    answer,
                    proposal_id: proposal_id.map(str::to_string),
                })
                .await
                .unwrap_err();
            assert!(err.downcast_ref::<InvalidInput>().is_some(), "{err}");
        }
        let long = "x".repeat(MAX_REQUEST_CHARS + 1);
        assert!(svc.update(request(&long)).await.is_err());
        assert!(seen.lock().unwrap().is_empty());
    }
}
