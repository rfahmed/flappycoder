use super::{TokenStream, LlmProvider};
use anyhow::Result;
use async_stream::stream;
use std::time::Duration;

pub struct Mock;

impl Default for Mock {
    fn default() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl LlmProvider for Mock {
    async fn stream_reply(
        &self,
        _model: &str,
        _history: &[(String, String)],
        _user_msg: &str,
    ) -> Result<TokenStream> {
        let s = stream! {
            for word in ["Hello", "from", "Mock", "LLM!"].into_iter() {
                tokio::time::sleep(Duration::from_millis(300)).await;
                yield word.to_string();
            }
        };
        Ok(Box::pin(s))
    }

    async fn list_models(&self) -> Result<Vec<String>> {
        Ok(vec![
            "gpt-3.5-turbo".to_string(),
            "gpt-4".to_string(),
            "claude-2".to_string(),
        ])
    }
} 