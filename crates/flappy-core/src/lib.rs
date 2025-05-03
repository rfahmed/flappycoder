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

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChatLogItem {
    Message(Role, String),
    EditedMessage(Role, String, usize, usize), // role, current text, add count, del count
    ModelSwitch(String), // Contains the name of the new model
    InitialModel(String), // For the very first message
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
    pub messages: Vec<ChatLogItem>,
    pub selected_message_index: Option<usize>, // Index of the message selected in chat view
    // Edit mode state
    pub editing: bool,                        // Whether we're in edit mode
    pub edit_buffer: String,                  // Current text being edited
    pub edit_cursor: usize,                   // Cursor position in edit_buffer
    pub original_text: String,                // Original message text before any edits
    pub original_index: Option<usize>,
}

impl App {
    pub fn new(llm: Arc<dyn LlmProvider>, model: impl Into<String>) -> Self {
        let (tx, rx) = async_channel::unbounded();
        let model_name = model.into(); // Convert model name early

        // Create the initial message using the new variant
        let initial_message = ChatLogItem::InitialModel(
            format!("--- Started with model: {} ---", model_name)
        );

        Self {
            tx_ui: tx,
            rx_ui: rx,
            llm,
            model: model_name,
            messages: vec![initial_message],
            focus: Focus::Input,
            chat_scroll: 0,
            selected_message_index: None, // Initially nothing selected
            editing: false,
            edit_buffer: String::new(),
            edit_cursor: 0,
            original_text: String::new(),
            original_index: None,
        }
    }

    pub async fn handle_user_msg(&mut self, txt: String) -> Result<()> {
        self.messages.push(ChatLogItem::Message(Role::User, txt.clone()));
        // Convert history to plain tuples to avoid flappy-llm depending on ChatLogItem
        let history: Vec<(String, String)> = self
            .messages
            .iter()
            // Skip the last message because it's the user message we are processing (txt)
            // and also filter out non-message items
            .rev()
            .skip(1)
            .rev()
            .filter_map(|item| match item {
                ChatLogItem::Message(role, content) => {
                    let role_str = match role {
                        Role::User => "user".to_string(),
                        Role::Assistant => "assistant".to_string(),
                    };
                    Some((role_str, content.clone()))
                }
                _ => None, // Filter out ModelSwitch and InitialModel
            })
            .collect();

        // Call with model, history, and the new user message (txt)
        let mut stream = self
            .llm
            .stream_reply(&self.model, &history, &txt)
            .await?;
        while let Some(tok) = stream.next().await {
            self.tx_ui.send(UiEvent::Token(Role::Assistant, tok)).await?;
        }
        Ok(())
    }

    // Start editing the selected message
    pub fn start_editing(&mut self) -> bool {
        if let Some(idx) = self.selected_message_index {
            if let ChatLogItem::Message(_, content) = &self.messages[idx] {
                if self.original_index != Some(idx) {
                    self.original_index = Some(idx);
                    self.original_text = content.clone();
                }
                self.editing = true;
                self.edit_buffer = content.clone();
                self.edit_cursor = self.edit_buffer.len(); // Place cursor at end
                return true;
            }
        }
        false // Can't edit if no message selected or not a Message type
    }

    // Accept edits and update the message
    pub fn commit_edit(&mut self) {
        if self.editing {
            if let Some(idx) = self.selected_message_index {
                if let ChatLogItem::Message(role, _) = &self.messages[idx] {
                    let role_clone = *role;
                    let (add, del) = compute_diff_counts(&self.original_text, &self.edit_buffer);
                    self.messages[idx] = ChatLogItem::EditedMessage(role_clone, self.edit_buffer.clone(), add, del);
                }
            }
            self.cancel_edit(); // Reset edit state
        }
    }

    // Cancel edits and revert to original
    pub fn cancel_edit(&mut self) {
        self.editing = false;
        self.edit_buffer.clear();
        self.edit_cursor = 0;
    }

    // Insert a character at the cursor position
    pub fn insert_char(&mut self, c: char) {
        if self.editing {
            self.edit_buffer.insert(self.edit_cursor, c);
            self.edit_cursor += 1;
        }
    }

    // Delete a character before the cursor
    pub fn delete_char(&mut self) {
        if self.editing && self.edit_cursor > 0 {
            self.edit_buffer.remove(self.edit_cursor - 1);
            self.edit_cursor -= 1;
        }
    }

    // Move cursor left
    pub fn cursor_left(&mut self) {
        if self.editing && self.edit_cursor > 0 {
            self.edit_cursor -= 1;
        }
    }

    // Move cursor right
    pub fn cursor_right(&mut self) {
        if self.editing && self.edit_cursor < self.edit_buffer.len() {
            self.edit_cursor += 1;
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Focus {
    Chat,
    Model,
    #[default]
    Input,
}

fn compute_diff_counts(orig: &str, edit: &str) -> (usize, usize) {
    let mut add = 0;
    let mut del = 0;
    let a_words: Vec<&str> = orig.split_whitespace().collect();
    let b_words: Vec<&str> = edit.split_whitespace().collect();
    let mut i = 0;
    let mut j = 0;
    while i < a_words.len() && j < b_words.len() {
        if a_words[i] == b_words[j] {
            i += 1;
            j += 1;
        } else {
            del += a_words[i].chars().count();
            add += b_words[j].chars().count();
            i += 1;
            j += 1;
        }
    }
    while i < a_words.len() {
        del += a_words[i].chars().count();
        i += 1;
    }
    while j < b_words.len() {
        add += b_words[j].chars().count();
        j += 1;
    }
    (add, del)
} 