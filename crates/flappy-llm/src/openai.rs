#[cfg(feature = "openai")]
use anyhow::{Context, Result};
#[cfg(feature = "openai")]
use async_stream::stream;
#[cfg(feature = "openai")]
use reqwest::{Client, header};
#[cfg(feature = "openai")]
use serde::Deserialize;
use serde::Serialize;

#[cfg(feature = "openai")]
use super::{LlmProvider, TokenStream};

#[derive(Serialize)]
struct Message {
    role: String,
    content: String,
}

#[derive(Serialize)]
struct ChatCompletionRequest {
    model: String,
    messages: Vec<Message>,
    stream: bool,
}

#[cfg(feature = "openai")]
#[derive(Clone)]
pub struct OpenAi {
    client: Client,
    _api_key: String,
    model: String,
    base_url: String,
}

#[cfg(feature = "openai")]
impl OpenAi {
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Result<Self> {
        let api_key = api_key.into();
        let mut headers = header::HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            format!("Bearer {api_key}").parse().unwrap(),
        );
        headers.insert(header::CONTENT_TYPE, "application/json".parse().unwrap());

        let client = Client::builder()
            .default_headers(headers)
            .build()?;
        Ok(Self { client, _api_key: api_key, model: model.into(), base_url: "https://api.openai.com/v1".to_string() })
    }

    fn chat_url(&self) -> String {
        format!("{}/chat/completions", self.base_url)
    }
}

#[cfg(feature = "openai")]
#[derive(Debug, Deserialize)]
struct ChatChunkChoiceDelta {
    content: Option<String>,
}

#[cfg(feature = "openai")]
#[derive(Debug, Deserialize)]
struct ChatChunkChoice {
    delta: ChatChunkChoiceDelta,
}

#[cfg(feature = "openai")]
#[derive(Debug, Deserialize)]
struct ChatChunk {
    choices: Vec<ChatChunkChoice>,
}

#[cfg(feature = "openai")]
#[async_trait::async_trait]
impl LlmProvider for OpenAi {
    async fn stream_reply(
        &self,
        model: &str,
        history: &[(String, String)],
        user_msg: &str,
    ) -> Result<TokenStream> {
        let url = format!("{}/chat/completions", self.base_url);

        let mut messages: Vec<Message> = history
            .iter()
            .map(|(role, content)| Message {
                role: role.clone(),
                content: content.clone(),
            })
            .collect();

        messages.push(Message {
            role: "user".to_string(),
            content: user_msg.to_string(),
        });

        let body = ChatCompletionRequest {
            model: model.to_string(),
            messages,
            stream: true,
        };

        let resp = self.client.post(&url).json(&body).send().await?;

        let mut stream_bytes = resp.bytes_stream();
        let s = stream! {
            use futures_util::StreamExt;
            while let Some(chunk) = stream_bytes.next().await {
                let chunk = match chunk {
                    Ok(c) => c,
                    Err(_) => continue,
                };
                let text = match std::str::from_utf8(&chunk) {
                    Ok(t) => t,
                    Err(_) => continue,
                };
                for line in text.split('\n') {
                    let line = line.trim();
                    if line.is_empty() { continue; }
                    if line == "data: [DONE]" { return; }
                    let json_part = line.strip_prefix("data: ").unwrap_or(line);
                    if let Ok(chunk_obj) = serde_json::from_str::<ChatChunk>(json_part) {
                        for choice in chunk_obj.choices {
                            if let Some(content) = choice.delta.content {
                                yield content;
                            }
                        }
                    }
                }
            }
        };
        Ok(Box::pin(s) as TokenStream)
    }

    async fn list_models(&self) -> Result<Vec<String>> {
        let url = format!("{}/models", self.base_url);
        let resp = self.client.get(&url)
            .send()
            .await
            .context("failed to request models list")?;
        #[derive(serde::Deserialize)]
        struct ModelInfo { id: String }
        #[derive(serde::Deserialize)]
        struct ModelsResponse { data: Vec<ModelInfo> }
        let body: ModelsResponse = resp.json().await.context("failed to parse models response")?;
        Ok(body.data.into_iter().map(|m| m.id).collect())
    }
} 