use anyhow::Result;
use async_channel::{Receiver, Sender};
use std::sync::Arc;

use futures_util::StreamExt;

pub use flappy_llm::{LlmProvider, TokenStream};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    User,
    Assistant,
}

pub enum UiEvent {
    Token(Role, String),
}

#[derive(Clone)]
pub struct App {
    pub tx_ui: Sender<UiEvent>,
    pub rx_ui: Receiver<UiEvent>,
    pub llm: Arc<dyn LlmProvider>,
    pub model: String,
    pub focus: Focus,
    pub chat_scroll: u16,
    pub selector_open: bool,
    pub messages: Vec<(Role, String)>,
}

impl App {
    pub fn new(llm: Arc<dyn LlmProvider>, model: impl Into<String>) -> Self {
        let (tx, rx) = async_channel::unbounded();
        Self {
            tx_ui: tx,
            rx_ui: rx,
            llm,
            model: model.into(),
            messages: Vec::new(),
            focus: Focus::Input,
            chat_scroll: 0,
            selector_open: false,
        }
    }

    pub async fn handle_user_msg(&mut self, txt: String) -> Result<()> {
        self.messages.push((Role::User, txt.clone()));
        let mut stream = self.llm.stream_reply(&txt).await?;
        while let Some(tok) = stream.next().await {
            self.tx_ui.send(UiEvent::Token(Role::Assistant, tok)).await?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Focus {
    Chat,
    Model,
    #[default]
    Input,
} 