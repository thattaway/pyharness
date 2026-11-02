//! Real-provider stream adapter: ph-ai completion streaming bridged into
//! the ph-agent loop's `StreamFn`/`ModelStream`, crossing the crate
//! boundary by wire-shape (JSON) round-trip; shared by ph-cli and
//! ph-daemon.

use std::sync::Arc;

use ph_agent::stream::{LlmContext, ModelStream, StreamFn, StreamRequestOptions};
use ph_agent::types::{Model as AgentModel, ThinkingLevel};
use ph_types::ai::Model;

/// Wire-shape conversion at the ph-agent/ph-ai boundary: both sides
/// serialize to the same camelCase wire shapes.
pub fn json_round_trip<T, U>(value: &T) -> Option<U>
where
    T: serde::Serialize,
    U: serde::de::DeserializeOwned,
{
    serde_json::to_value(value)
        .ok()
        .and_then(|value| serde_json::from_value(value).ok())
}

/// Thinking-level mapping across the two crates.
#[must_use]
pub fn map_thinking_level(level: ph_types::ai::ModelThinkingLevel) -> ThinkingLevel {
    match level {
        ph_types::ai::ModelThinkingLevel::Off => ThinkingLevel::Off,
        ph_types::ai::ModelThinkingLevel::Minimal => ThinkingLevel::Minimal,
        ph_types::ai::ModelThinkingLevel::Low => ThinkingLevel::Low,
        ph_types::ai::ModelThinkingLevel::Medium => ThinkingLevel::Medium,
        ph_types::ai::ModelThinkingLevel::High => ThinkingLevel::High,
        ph_types::ai::ModelThinkingLevel::Xhigh => ThinkingLevel::Xhigh,
        ph_types::ai::ModelThinkingLevel::Max => ThinkingLevel::Max,
    }
}

/// The inverse of [`map_thinking_level`]: the ph-types view of the agent
/// state's thinking level.
#[must_use]
pub fn model_thinking_level(level: ThinkingLevel) -> ph_types::ai::ModelThinkingLevel {
    match level {
        ThinkingLevel::Off => ph_types::ai::ModelThinkingLevel::Off,
        ThinkingLevel::Minimal => ph_types::ai::ModelThinkingLevel::Minimal,
        ThinkingLevel::Low => ph_types::ai::ModelThinkingLevel::Low,
        ThinkingLevel::Medium => ph_types::ai::ModelThinkingLevel::Medium,
        ThinkingLevel::High => ph_types::ai::ModelThinkingLevel::High,
        ThinkingLevel::Xhigh => ph_types::ai::ModelThinkingLevel::Xhigh,
        ThinkingLevel::Max => ph_types::ai::ModelThinkingLevel::Max,
    }
}

/// Adapt the loop-level payload hook to the ph-ai hook shape. A failed
/// round-trip keeps the payload unchanged — hooks never fail the request.
fn agent_payload_hook_to_ai(hook: ph_agent::stream::OnPayloadHook) -> ph_ai::types::OnPayloadHook {
    std::sync::Arc::new(move |payload: serde_json::Value, model: &Model| {
        match json_round_trip::<_, ph_agent::types::Model>(model) {
            Some(agent_model) => hook(payload, &agent_model),
            None => Some(payload),
        }
    })
}

/// Adapt the loop-level response hook to the ph-ai hook shape; a failed
/// round-trip drops the hook call (advisory, never fatal).
fn agent_response_hook_to_ai(
    hook: ph_agent::stream::OnResponseHook,
) -> ph_ai::types::OnResponseHook {
    std::sync::Arc::new(
        move |response: ph_ai::types::ProviderResponse, model: &Model| {
            let agent_response = ph_agent::stream::ProviderResponse {
                status: response.status,
                headers: response.headers,
            };
            if let Some(agent_model) = json_round_trip::<_, ph_agent::types::Model>(model) {
                hook(agent_response, &agent_model);
            }
        },
    )
}

/// The mutable provider target a live session's stream reads per call
/// (daemon `set_model` and the failover switch swap it).
#[derive(Debug, Clone)]
pub struct ProviderTarget {
    pub api_key: Option<String>,
    pub model: Model,
    pub service_tier: Option<ph_types::ai::ServiceTier>,
    /// The auth-resolved provider-request headers: a live `set_model` carries
    /// them through to the stream like the build-time resolution does.
    pub headers: Option<std::collections::BTreeMap<String, String>>,
}

/// A real ph-ai stream adapter reading its target from a shared slot the host can swap live
/// (`set_model`, failover); `None` only before the host sets the build-time target.
///
/// # Panics
///
/// Panics at stream time if the provider target lock is poisoned, or if the
/// target slot was never set before the first stream.
pub fn switchable_stream_fn(target: Arc<std::sync::RwLock<Option<ProviderTarget>>>) -> StreamFn {
    Arc::new(
        move |_requested: AgentModel, context: LlmContext, options: StreamRequestOptions| {
            let ProviderTarget {
                api_key,
                model,
                service_tier,
                headers,
            } = target
                .read()
                .expect("provider target lock")
                .clone()
                .expect("provider target set before the first stream");
            Box::pin(async move {
                stream_once(&model, api_key, service_tier, headers, context, options)
            })
        },
    )
}

/// Stream one completion against `model` with `api_key` and the auth-resolved
/// request `headers`: the per-request tail the switchable seams and the CLI's
/// route-authoritative variant share.
///
/// # Errors
///
/// Returns the provider stream's error when the request fails (the
/// per-attempt failures the retry driver classifies).
pub fn stream_once(
    model: &Model,
    api_key: Option<String>,
    service_tier: Option<ph_types::ai::ServiceTier>,
    headers: Option<std::collections::BTreeMap<String, String>>,
    context: LlmContext,
    options: StreamRequestOptions,
) -> anyhow::Result<Box<dyn ModelStream>> {
    let messages: Vec<ph_types::ai::Message> = context
        .messages
        .iter()
        .filter_map(json_round_trip)
        .collect();
    let tools: Vec<ph_types::ai::Tool> = context.tools.iter().filter_map(json_round_trip).collect();
    let ai_context = ph_types::ai::Context {
        system_prompt: context.system_prompt,
        messages,
        tools: Some(tools),
    };
    // The turn's abort signal reaches the transport: the in-flight request races this
    // token; the loop's abort paths fire it through [`ModelStream::close`] or the drop.
    let cancel = tokio_util::sync::CancellationToken::new();
    let stream_options = ph_ai::types::SimpleStreamOptions {
        base: ph_ai::types::StreamOptions {
            temperature: options.temperature,
            max_tokens: options.max_tokens,
            signal: Some(cancel.clone()),
            api_key,
            // A request that pins its transport (side questions ride SSE so they stay off the
            // session's Codex socket and its continuation) carries it through.
            transport: options.transport.as_ref().and_then(json_round_trip),
            service_tier,
            cache_retention: None,
            session_id: options.session_id.clone(),
            // The loop-level request hooks cross the crate boundary here: the payload
            // hook may replace the wire payload, the response hook observes headers.
            on_payload: options.on_payload.map(agent_payload_hook_to_ai),
            on_response: options.on_response.map(agent_response_hook_to_ai),
            // StreamOptions carries a plain map; the target's ordered
            // (BTreeMap) resolution converts here, with the request's own
            // headers (the semantic request id) merged over it — TS
            // providers' `mergeHeaders(..., optionsHeaders)` order, the
            // options win.
            headers: {
                let mut merged = headers;
                if let Some(request_headers) = options.headers.clone() {
                    merged
                        .get_or_insert_with(std::collections::BTreeMap::new)
                        .extend(request_headers);
                }
                merged.map(|headers| headers.into_iter().collect())
            },
            metadata: None,
            timeout_ms: None,
        },
        reasoning: Some(model_thinking_level(options.reasoning)),
        thinking_budgets: None,
    };
    let stream = ph_ai::stream_simple(model, &ai_context, Some(stream_options))
        .map_err(|error| anyhow::anyhow!("{error:?}"))?;
    // Pump ph-ai events into the loop's event stream, verbatim.
    let (handle, consumer) = ph_agent::stream::event_stream();
    let forwarder = tokio::spawn(async move {
        let mut stream = stream;
        while let Some(event) = stream.next_event().await {
            if let Some(converted) = convert_stream_event(&event) {
                handle.push(converted);
            }
        }
        let result = stream.result().await;
        if let Some(converted) = json_round_trip::<_, ph_agent::types::AssistantMessage>(&result) {
            handle.end(Some(converted));
        } else {
            handle.end(None);
        }
    });
    // Keep the pump task alive as long as the stream lives.
    let (forwarder, consumer) = (forwarder, consumer);
    Ok(consumer_pump(forwarder, consumer, cancel))
}

/// A stream adapter pinned to one target (headless runtimes resolve
/// their model once, so the slot never changes).
#[must_use]
pub fn real_stream_fn(api_key: Option<String>, model: Model) -> StreamFn {
    switchable_stream_fn(Arc::new(std::sync::RwLock::new(Some(ProviderTarget {
        api_key,
        model,
        service_tier: None,
        headers: None,
    }))))
}

/// Convert one ph-ai stream event into the ph-agent loop's event enum.
/// Payloads cross the boundary by wire-shape (JSON) round-trip.
///
/// # Panics
///
/// Panics when an assistant message cannot round-trip across the two
/// crates' wire shapes (a structural shape-mismatch bug).
#[must_use]
pub fn convert_stream_event(
    event: &ph_types::ai::AssistantMessageEvent,
) -> Option<ph_agent::stream::AssistantMessageEvent> {
    use ph_agent::stream::AssistantMessageEvent as Out;
    use ph_types::ai::AssistantMessageEvent as In;
    fn convert_partial(
        message: &ph_types::ai::AssistantMessage,
    ) -> ph_agent::types::AssistantMessage {
        json_round_trip(message).expect("assistant wire shapes match")
    }
    Some(match event {
        In::Start { partial } => Out::Start {
            partial: convert_partial(partial),
        },
        In::TextStart {
            content_index,
            partial,
        } => Out::TextStart {
            content_index: *content_index as usize,
            partial: convert_partial(partial),
        },
        In::TextDelta {
            content_index,
            delta,
            partial,
        } => Out::TextDelta {
            content_index: *content_index as usize,
            delta: delta.clone(),
            partial: convert_partial(partial),
        },
        In::TextEnd {
            content_index,
            content,
            partial,
        } => Out::TextEnd {
            content_index: *content_index as usize,
            content: content.clone(),
            partial: convert_partial(partial),
        },
        In::ThinkingStart {
            content_index,
            partial,
        } => Out::ThinkingStart {
            content_index: *content_index as usize,
            partial: convert_partial(partial),
        },
        In::ThinkingDelta {
            content_index,
            delta,
            partial,
        } => Out::ThinkingDelta {
            content_index: *content_index as usize,
            delta: delta.clone(),
            partial: convert_partial(partial),
        },
        In::ThinkingEnd {
            content_index,
            partial,
            ..
        } => Out::ThinkingEnd {
            content_index: *content_index as usize,
            partial: convert_partial(partial),
        },
        In::ToolcallStart {
            content_index,
            partial,
        } => Out::ToolCallStart {
            content_index: *content_index as usize,
            partial: convert_partial(partial),
        },
        In::ToolcallDelta {
            content_index,
            delta,
            partial,
        } => Out::ToolCallDelta {
            content_index: *content_index as usize,
            delta: delta.clone(),
            partial: convert_partial(partial),
        },
        In::ToolcallEnd {
            content_index,
            tool_call,
            partial,
        } => Out::ToolCallEnd {
            content_index: *content_index as usize,
            tool_call: json_round_trip(tool_call).expect("tool call wire shapes match"),
            partial: convert_partial(partial),
        },
        In::Done { reason, message } => Out::Done {
            reason: json_round_trip(reason).expect("stop reason wire shapes match"),
            message: convert_partial(message),
        },
        In::Error { reason, error } => Out::Error {
            reason: json_round_trip(reason).expect("stop reason wire shapes match"),
            error: convert_partial(error),
        },
    })
}

/// Wrap the consumer so the pump task is aborted when the stream drops.
fn consumer_pump(
    forwarder: tokio::task::JoinHandle<()>,
    consumer: ph_agent::stream::AssistantMessageEventStream,
    cancel: tokio_util::sync::CancellationToken,
) -> Box<dyn ModelStream> {
    Box::new(PumpedStream {
        _forwarder: forwarder,
        stream: consumer,
        cancel,
    })
}

/// A `ModelStream` whose lifetime keeps the ph-ai pump task alive and owns
/// the fetch's cancellation token (the transport half of the turn-abort).
struct PumpedStream {
    _forwarder: tokio::task::JoinHandle<()>,
    stream: ph_agent::stream::AssistantMessageEventStream,
    cancel: tokio_util::sync::CancellationToken,
}

impl Drop for PumpedStream {
    fn drop(&mut self) {
        // The in-flight fetch cancels instead of running to completion
        // detached (TS: the fetch dies with its iterator).
        self.cancel.cancel();
    }
}

impl ModelStream for PumpedStream {
    fn next_event(
        &mut self,
    ) -> ph_agent::BoxFut<'_, Option<ph_agent::stream::AssistantMessageEvent>> {
        self.stream.next_event()
    }

    fn result(
        &mut self,
    ) -> ph_agent::BoxFut<'_, anyhow::Result<ph_agent::types::AssistantMessage>> {
        self.stream.result()
    }

    /// Close/cancel: the in-flight fetch cancels immediately. Idempotent
    /// — the token's cancelled state is sticky.
    fn close(&mut self) {
        self.cancel.cancel();
    }
}

#[cfg(test)]
mod tests {
    //! Regression guard for the ph-agent -> ph-ai message boundary: `UserPart`
    //! must stay `type`-tagged like the TS wire format; an untagged variant
    //! silently dropped every `AgentPromptInput::Text` prompt, leaving the
    //! provider with a system prompt only.

    use super::*;

    #[tokio::test]
    async fn live_target_service_tier_reaches_provider_and_reset() {
        let registration =
            ph_ai::faux::register_faux_provider(ph_ai::faux::RegisterFauxProviderOptions {
                api: Some("restored-service-tier-test".to_owned()),
                ..Default::default()
            });
        let received = Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured = received.clone();
        let factory =
            ph_ai::faux::FauxResponseStep::Factory(Arc::new(move |_, options, _, model| {
                captured.lock().unwrap().push((
                    model.id.clone(),
                    options.and_then(|options| options.service_tier),
                ));
                Ok(ph_ai::faux::faux_assistant_text_message(
                    "ok",
                    ph_ai::faux::FauxAssistantMessageOptions::default(),
                ))
            }));
        registration.set_responses(vec![factory.clone(), factory]);
        let model = registration.get_model();
        let target = Arc::new(std::sync::RwLock::new(Some(ProviderTarget {
            api_key: None,
            model: model.clone(),
            service_tier: Some(ph_types::ai::ServiceTier::Priority),
            headers: None,
        })));
        let stream_fn = switchable_stream_fn(target.clone());
        for tier in [Some(ph_types::ai::ServiceTier::Priority), None] {
            target.write().unwrap().as_mut().unwrap().service_tier = tier;
            let mut stream = stream_fn(
                AgentModel::unknown(),
                LlmContext::default(),
                StreamRequestOptions::default(),
            )
            .await
            .unwrap();
            stream.result().await.unwrap();
        }
        assert_eq!(
            *received.lock().unwrap(),
            vec![
                (model.id.clone(), Some(ph_types::ai::ServiceTier::Priority)),
                (model.id, None),
            ]
        );
        registration.unregister();
    }

    #[tokio::test]
    async fn requested_transport_reaches_the_provider() {
        let registration =
            ph_ai::faux::register_faux_provider(ph_ai::faux::RegisterFauxProviderOptions {
                api: Some("requested-transport-test".to_owned()),
                ..Default::default()
            });
        let received = Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured = received.clone();
        let factory = ph_ai::faux::FauxResponseStep::Factory(Arc::new(move |_, options, _, _| {
            captured
                .lock()
                .unwrap()
                .push(options.and_then(|options| options.transport));
            Ok(ph_ai::faux::faux_assistant_text_message(
                "ok",
                ph_ai::faux::FauxAssistantMessageOptions::default(),
            ))
        }));
        registration.set_responses(vec![factory.clone(), factory]);
        let model = registration.get_model();
        for transport in [Some(ph_agent::types::Transport::Sse), None] {
            let mut stream = stream_once(
                &model,
                None,
                None,
                None,
                LlmContext::default(),
                StreamRequestOptions {
                    transport,
                    ..Default::default()
                },
            )
            .unwrap();
            stream.result().await.unwrap();
        }
        assert_eq!(
            *received.lock().unwrap(),
            vec![Some(ph_types::ai::Transport::Sse), None]
        );
        registration.unregister();
    }

    #[test]
    fn prompt_text_message_round_trips() {
        let message = ph_agent::types::AgentMessage::Standard(ph_agent::types::Message::User(
            ph_agent::types::UserMessage {
                content: ph_agent::types::UserContent::Parts(vec![
                    ph_agent::types::UserPart::Text(ph_agent::types::TextContent {
                        text: "reply with ok".into(),
                        text_signature: None,
                    }),
                ]),
                timestamp: 1,
            },
        ));
        let converted: Option<ph_types::ai::Message> = json_round_trip(&message);
        assert_eq!(
            converted,
            Some(ph_types::ai::Message::User(ph_types::ai::UserMessage {
                content: ph_types::ai::UserContent::Blocks(vec![
                    ph_types::ai::UserContentBlock::Text(ph_types::ai::TextContent {
                        text: "reply with ok".into(),
                        text_signature: None,
                        rest: serde_json::Map::default(),
                    }),
                ]),
                timestamp: 1,
                rest: serde_json::Map::default(),
            }))
        );
    }

    /// Regression guard: provider signatures must survive the round-trip. ph-agent has no
    /// catch-all field, so a key-casing mismatch silently dropped them — a Rust-written
    /// session lost the signature TS-written ones carry.
    #[test]
    fn provider_signatures_round_trip_both_directions() {
        let thinking = ph_types::ai::ThinkingContent {
            thinking: "trace".into(),
            thinking_signature: Some("sig-1".into()),
            redacted: None,
            rest: serde_json::Map::default(),
        };
        let wire = serde_json::to_value(&thinking).unwrap();
        assert_eq!(
            wire.get("thinkingSignature").and_then(|v| v.as_str()),
            Some("sig-1"),
            "the TS wire key is camelCase: {wire}"
        );
        // ph-ai stream output -> the ph-agent loop's message form.
        let agent_thinking: ph_agent::types::ThinkingContent =
            serde_json::from_value(wire).unwrap();
        assert_eq!(agent_thinking.thinking_signature.as_deref(), Some("sig-1"));
        // The loop's message -> the provider-facing ph-ai form again.
        let back: ph_types::ai::ThinkingContent =
            serde_json::from_value(serde_json::to_value(&agent_thinking).unwrap()).unwrap();
        assert_eq!(back.thinking_signature.as_deref(), Some("sig-1"));
        // The tool-call thought signature (Google) rides the same boundary.
        let tool_call = ph_types::ai::ToolCall {
            id: "toolu_1".into(),
            name: "bash".into(),
            arguments: serde_json::Map::new(),
            thought_signature: Some("sig-2".into()),
            rest: serde_json::Map::default(),
        };
        let wire = serde_json::to_value(&tool_call).unwrap();
        assert_eq!(
            wire.get("thoughtSignature").and_then(|v| v.as_str()),
            Some("sig-2"),
            "the TS wire key is camelCase: {wire}"
        );
        let agent_tool_call: ph_agent::types::ToolCall = serde_json::from_value(wire).unwrap();
        assert_eq!(agent_tool_call.thought_signature.as_deref(), Some("sig-2"));
    }

    /// A delayed faux response holds the request mid-wait; `ModelStream::close` cancels the fetch
    /// NOW, so the stream settles on the aborted message instead of waiting out the hold.
    #[tokio::test]
    async fn closing_the_stream_cancels_a_held_fetch_immediately() {
        let registration =
            ph_ai::faux::register_faux_provider(ph_ai::faux::RegisterFauxProviderOptions {
                api: Some("held-fetch-close-test".to_string()),
                ..Default::default()
            });
        let model = registration.get_model();
        registration.set_responses(vec![ph_ai::faux::FauxResponseStep::Delayed {
            message: ph_ai::faux::faux_assistant_text_message(
                "held reply",
                ph_ai::faux::FauxAssistantMessageOptions::default(),
            ),
            delay_ms: 60_000,
        }]);
        let target = Arc::new(std::sync::RwLock::new(Some(ProviderTarget {
            api_key: None,
            model: model.clone(),
            service_tier: None,
            headers: None,
        })));
        let stream_fn = switchable_stream_fn(target);
        let mut stream = stream_fn(
            ph_agent::types::Model::unknown(),
            LlmContext::default(),
            StreamRequestOptions::default(),
        )
        .await
        .expect("stream start");
        // Abort the turn mid-wait.
        stream.close();
        let settled = tokio::time::timeout(std::time::Duration::from_secs(5), stream.result())
            .await
            .expect("the closed stream settles immediately, not after the 60s hold")
            .expect("stream result");
        assert_eq!(settled.stop_reason, ph_agent::types::StopReason::Aborted);
        assert_eq!(
            settled.error_message.as_deref(),
            Some("Request was aborted")
        );
        // No usage on a mid-wait abort (TS `EMPTY_USAGE`: no partial
        // message ever streamed).
        let usage = settled.usage;
        assert_eq!(usage.total_tokens, 0);
        assert_eq!(usage.input, 0);
        assert_eq!(usage.output, 0);
        assert_eq!(usage.cost.total, 0.0);
        registration.unregister();
    }
}
