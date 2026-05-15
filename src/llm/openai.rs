// Copyright (C) 2026 The orangu community
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program. If not, see <https://www.gnu.org/licenses/>.

use super::{ChatMessage, LlmResponse, ToolCall, ToolCallFunction, ToolDefinition};
use crate::config::LlmConfiguration;
use anyhow::{Result, anyhow};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, time::Duration};

pub struct OpenAiClient {
    http_client: Client,
    endpoint: String,
    model: String,
    api_key: Option<String>,
}

#[derive(Debug, Serialize)]
struct OpenAiChatRequest<'a> {
    model: &'a str,
    messages: &'a [ChatMessage],
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<&'a [ToolDefinition]>,
}

#[derive(Debug, Deserialize)]
struct OpenAiChatResponse {
    choices: Vec<OpenAiChoice>,
}

#[derive(Debug, Deserialize)]
struct OpenAiChoice {
    message: OpenAiMessage,
}

#[derive(Debug, Deserialize)]
struct OpenAiMessage {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<OpenAiToolCall>>,
}

#[derive(Debug, Deserialize)]
struct OpenAiToolCall {
    #[serde(default)]
    id: Option<String>,
    #[serde(rename = "type", default)]
    tool_type: Option<String>,
    function: OpenAiToolCallFunction,
}

#[derive(Debug, Deserialize)]
struct OpenAiToolCallFunction {
    name: String,
    arguments: String,
}

impl OpenAiClient {
    pub fn from_profile(profile: &LlmConfiguration) -> Result<Self> {
        Ok(Self {
            http_client: Client::builder()
                .timeout(Duration::from_secs(profile.request_timeout_seconds))
                .build()?,
            endpoint: normalize_openai_endpoint(&profile.endpoint),
            model: profile.model.clone(),
            api_key: profile.api_key.clone(),
        })
    }

    pub async fn chat(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
    ) -> Result<LlmResponse> {
        let url = format!("{}/v1/chat/completions", self.endpoint);
        let request = OpenAiChatRequest {
            model: &self.model,
            messages,
            stream: false,
            tools: if tools.is_empty() { None } else { Some(tools) },
        };

        let mut builder = self.http_client.post(&url).json(&request);
        if let Some(api_key) = &self.api_key {
            builder = builder.bearer_auth(api_key);
        }

        let resp = builder.send().await.map_err(|e| {
            anyhow!(
                "failed to send chat request to {} using model {}: {}",
                self.endpoint,
                self.model,
                e
            )
        })?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "chat completion failed (status {}): {}",
                status,
                body
            ));
        }

        let chat_resp: OpenAiChatResponse = resp.json().await?;
        let message = chat_resp
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("server returned an empty choices list"))?
            .message;

        match message.tool_calls {
            Some(calls) if !calls.is_empty() => {
                let mapped_calls = calls
                    .into_iter()
                    .map(|tc| {
                        let arguments = parse_tool_arguments(&tc.function.arguments)?;
                        Ok(ToolCall {
                            id: tc
                                .id
                                .unwrap_or_else(|| format!("call_{}", tc.function.name)),
                            tool_type: tc.tool_type.unwrap_or_else(|| "function".to_string()),
                            function: ToolCallFunction {
                                name: tc.function.name,
                                arguments,
                            },
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;
                Ok(LlmResponse::ToolCalls(mapped_calls))
            }
            _ => Ok(LlmResponse::Text(message.content.unwrap_or_default())),
        }
    }
}

fn normalize_openai_endpoint(endpoint: &str) -> String {
    let endpoint = endpoint.trim_end_matches('/');
    endpoint.strip_suffix("/v1").unwrap_or(endpoint).to_string()
}

fn parse_tool_arguments(arguments: &str) -> Result<HashMap<String, serde_json::Value>> {
    let trimmed = arguments.trim();
    if trimmed.is_empty() {
        return Ok(HashMap::new());
    }
    Ok(serde_json::from_str(trimmed)?)
}
