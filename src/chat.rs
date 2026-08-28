//! The chat screen.
//!
//! Enter sends the whole transcript to the Anthropic Messages API and streams
//! the reply back into the last bubble, token by token. The shell around it
//! draws the title bar and the nav, so this view is only the transcript and
//! the composer.

use std::sync::Arc;

use futures::{AsyncBufReadExt as _, AsyncReadExt as _, StreamExt as _};
use gpui::prelude::FluentBuilder as _;
use gpui::*;
use gpui_component::{
    input::{InputEvent, Textarea, TextareaState},
    scroll::ScrollableElement as _,
    text::TextView,
    v_flex,
};
use http_client::{AsyncBody, HttpClient, Method, Request, StatusCode};
use magic_carpet_chat::secrets::Secret;
use serde_json::json;

use crate::palette::*;

const MODEL: &str = "claude-sonnet-5";
/// The reply ceiling. Streaming makes a large value safe (no HTTP timeout), and
/// a small one truncates long answers mid-sentence.
const MAX_TOKENS: u32 = 64_000;
const API_URL: &str = "https://api.anthropic.com/v1/messages";

const NO_KEY: &str = "**No API key.** Set `ANTHROPIC_API_KEY` in the shell that \
    launches this app, then start it again.";
const NO_KEY_CAUSE: &str = "ANTHROPIC_API_KEY is not set. Export it in the shell \
    that launches this app, then start it again.";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    User,
    Assistant,
}

struct Message {
    role: Role,
    /// Empty on an assistant message means the reply has not started yet, so
    /// the bubble shows "thinking…".
    text: String,
}

impl Message {
    fn user(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            text: text.into(),
        }
    }

    fn assistant(text: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            text: text.into(),
        }
    }
}

pub struct Chat {
    messages: Vec<Message>,
    /// Index of the bubble the model is filling in, if a reply is in flight.
    streaming: Option<usize>,
    composer: Entity<TextareaState>,
    scroll: ScrollHandle,
    /// A gpui Task is cancel-on-drop, so the live request has to be held here.
    _stream: Option<Task<()>>,
    /// Held so the composer subscription lives as long as the view.
    _subscriptions: Vec<Subscription>,
}

impl Chat {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let composer = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(1, 6)
                .submit_on_enter(true)
                .placeholder("Message Claude…")
        });

        let subscription = cx.subscribe_in(&composer, window, {
            move |this: &mut Self, composer, event: &InputEvent, window, cx| {
                let InputEvent::PressEnter { shift, .. } = event else {
                    return;
                };
                if *shift || this.streaming.is_some() {
                    return;
                }

                let text = composer.read(cx).value().trim().to_string();
                if text.is_empty() {
                    return;
                }

                composer.update(cx, |state, cx| state.set_value("", window, cx));
                this.send(text, cx);
            }
        });

        Self {
            // The transcript lives in memory only — quitting the app loses it.
            messages: match api_key() {
                Some(_) => Vec::new(),
                None => vec![Message::assistant(NO_KEY)],
            },
            streaming: None,
            composer,
            scroll: ScrollHandle::new(),
            _stream: None,
            _subscriptions: vec![subscription],
        }
    }

    fn send(&mut self, text: String, cx: &mut Context<Self>) {
        self.messages.push(Message::user(text));
        self.messages.push(Message::assistant(""));
        let reply_ix = self.messages.len() - 1;
        self.streaming = Some(reply_ix);
        self.scroll.scroll_to_bottom();
        cx.notify();

        let Some(api_key) = api_key() else {
            self.finish(Err(NO_KEY_CAUSE.to_string()), cx);
            return;
        };

        let payload = json!({
            "model": MODEL,
            "max_tokens": MAX_TOKENS,
            "stream": true,
            "messages": self
                .messages
                .iter()
                .filter(|message| !message.text.is_empty())
                .map(|message| json!({
                    "role": match message.role {
                        Role::User => "user",
                        Role::Assistant => "assistant",
                    },
                    "content": message.text,
                }))
                .collect::<Vec<_>>(),
        })
        .to_string();

        let http = cx.http_client();
        self._stream = Some(cx.spawn(async move |this, cx| {
            let result = stream_reply(http, api_key, payload, reply_ix, this.clone(), cx).await;
            this.update(cx, |this, cx| this.finish(result, cx)).ok();
        }));
    }

    fn push_chunk(&mut self, ix: usize, chunk: &str, cx: &mut Context<Self>) {
        if let Some(message) = self.messages.get_mut(ix) {
            message.text.push_str(chunk);
            self.scroll.scroll_to_bottom();
            cx.notify();
        }
    }

    fn finish(&mut self, result: Result<(), String>, cx: &mut Context<Self>) {
        let Some(ix) = self.streaming.take() else {
            return;
        };
        if let Some(message) = self.messages.get_mut(ix) {
            match result {
                Err(cause) => message.text = format!("**Something went wrong.** {cause}"),
                Ok(()) if message.text.is_empty() => {
                    message.text = "**The model sent an empty reply.**".to_string()
                }
                Ok(()) => {}
            }
        }
        self.scroll.scroll_to_bottom();
        cx.notify();
    }

    fn render_message(&self, ix: usize, message: &Message) -> impl IntoElement {
        let is_user = message.role == Role::User;

        div()
            .w_full()
            .flex()
            .when(is_user, |this| this.justify_end())
            .child(
                v_flex()
                    .max_w(relative(0.82))
                    .px(px(14.))
                    .py(px(10.))
                    .rounded(px(14.))
                    .border_1()
                    .map(|this| {
                        if is_user {
                            this.bg(grad(ACCENT, ACCENT_DEEP))
                                .border_color(rgb(ACCENT_DEEP))
                                .text_color(rgb(BG_RAIL))
                                .child(SharedString::from(message.text.clone()))
                        } else if message.text.is_empty() {
                            this.bg(rgb(BG_CARD))
                                .border_color(rgb(BORDER))
                                .text_color(rgb(TEXT_MUTED))
                                .child("thinking…")
                        } else {
                            this.bg(rgb(BG_CARD)).border_color(rgb(BORDER)).child(
                                TextView::markdown(
                                    ("message", ix),
                                    SharedString::from(message.text.clone()),
                                )
                                .selectable(true),
                            )
                        }
                    }),
            )
    }
}

impl Focusable for Chat {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.composer.focus_handle(cx)
    }
}

impl Render for Chat {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let transcript = v_flex().gap(px(12.)).py(px(4.)).children(
            self.messages
                .iter()
                .enumerate()
                .map(|(ix, message)| self.render_message(ix, message)),
        );

        v_flex()
            .size_full()
            .text_size(px(13.5))
            .child(
                // Transcript: a scroll area plus gpui-component's overlay scrollbar.
                // The handle is ours so a new message can pull the view to the bottom.
                div()
                    .relative()
                    .flex_1()
                    .min_h(px(0.))
                    .child(
                        div()
                            .id("transcript")
                            .size_full()
                            .track_scroll(&self.scroll)
                            .overflow_y_scroll()
                            .child(transcript),
                    )
                    .vertical_scrollbar(&self.scroll),
            )
            .child(
                v_flex()
                    .gap(px(6.))
                    .pt(px(12.))
                    .child(Textarea::new(&self.composer))
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(rgb(TEXT_DIM))
                            .child("Enter to send · Shift+Enter for a new line"),
                    ),
            )
    }
}

/// The API key is a bearer credential like the nsecs, so it travels in the
/// same redacting [`Secret`] wrapper: a stray `{:?}` on the request path can
/// never print it. It is exposed exactly once, at the header site.
fn api_key() -> Option<Secret> {
    // Trim: a key read from a file usually carries a newline, and a header value
    // with one fails to build with a message that says nothing about why.
    std::env::var("ANTHROPIC_API_KEY")
        .ok()
        .map(|key| key.trim().to_string())
        .filter(|key| !key.is_empty())
        .map(Secret::new)
}

/// POSTs the transcript and feeds every `text_delta` into the pending bubble.
/// Runs on gpui's foreground executor; the socket reads never block the frame.
async fn stream_reply(
    http: Arc<dyn HttpClient>,
    api_key: Secret,
    payload: String,
    reply_ix: usize,
    chat: WeakEntity<Chat>,
    cx: &mut AsyncApp,
) -> Result<(), String> {
    let request = Request::builder()
        .method(Method::POST)
        .uri(API_URL)
        .header("content-type", "application/json")
        .header("accept", "text/event-stream")
        .header("anthropic-version", "2023-06-01")
        .header("x-api-key", api_key.expose())
        .body(AsyncBody::from(payload))
        .map_err(|error| format!("The request would not build: {error}"))?;

    let mut response = http
        .send(request)
        .await
        .map_err(|error| format!("Could not reach api.anthropic.com: {error}"))?;

    if !response.status().is_success() {
        let status = response.status();
        let mut body = String::new();
        response.body_mut().read_to_string(&mut body).await.ok();
        return Err(explain(status, &body));
    }

    let mut lines = futures::io::BufReader::new(response.into_body()).lines();
    while let Some(line) = lines.next().await {
        let line = line.map_err(|error| format!("The stream broke: {error}"))?;
        let Some(data) = line.strip_prefix("data: ") else {
            continue;
        };
        let Ok(event) = serde_json::from_str::<serde_json::Value>(data) else {
            continue;
        };
        match event["type"].as_str() {
            Some("content_block_delta") => {
                if let Some(chunk) = event["delta"]["text"].as_str() {
                    let chunk = chunk.to_string();
                    chat.update(cx, |chat, cx| chat.push_chunk(reply_ix, &chunk, cx))
                        .map_err(|_| "The chat window closed.".to_string())?;
                }
            }
            // Without this a reply that hits the ceiling reads as a finished one.
            Some("message_delta") if event["delta"]["stop_reason"] == "max_tokens" => {
                let note = format!("\n\n_(cut off at the {MAX_TOKENS}-token limit)_");
                chat.update(cx, |chat, cx| chat.push_chunk(reply_ix, &note, cx))
                    .map_err(|_| "The chat window closed.".to_string())?;
            }
            Some("error") => {
                return Err(event["error"]["message"]
                    .as_str()
                    .unwrap_or("The API reported an error mid-stream.")
                    .to_string());
            }
            _ => {}
        }
    }

    Ok(())
}

fn explain(status: StatusCode, body: &str) -> String {
    let mut detail = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|value| value["error"]["message"].as_str().map(str::to_string))
        .unwrap_or_else(|| body.trim().to_string());
    // A gateway in front of the API answers with an HTML page, not JSON. Keep the
    // first line of it out of the bubble instead of the whole document.
    if detail.chars().count() > 300 {
        detail = detail.chars().take(300).chain(['…']).collect();
    }

    match status {
        StatusCode::UNAUTHORIZED => {
            format!("The API rejected the key in ANTHROPIC_API_KEY: {detail}")
        }
        StatusCode::TOO_MANY_REQUESTS => format!("Rate limited: {detail}"),
        _ => format!("The API answered {}: {detail}", status.as_u16()),
    }
}
