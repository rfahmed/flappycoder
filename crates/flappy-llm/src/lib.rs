pub mod mock;
#[cfg(feature = "openai")]
pub mod openai;

use std::pin::Pin;
use futures_core::Stream;
use anyhow::Result;
use async_trait::async_trait;

pub type TokenStream = Pin<Box<dyn Stream<Item = String> + Send>>;

/// The role string should be "user" or "assistant" in lower-case.
#[async_trait]
pub trait LlmProvider: Send + Sync {
    async fn stream_reply(
        &self,
        model: &str,
        history: &[(String, String)],
        user_msg: &str,
    ) -> Result<TokenStream>;
    /// Return a list of available model IDs.
    async fn list_models(&self) -> Result<Vec<String>>;
} 