use anyhow::Result;
use crossterm::event::{self, Event as CEvent, KeyCode, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use flappy_core::{App, UiEvent, Role, Focus, ChatLogItem};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Style, Color, Modifier};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph, Table, Row, Cell, Wrap, Scrollbar, ScrollbarOrientation, ScrollbarState};
use ratatui::prelude::Margin;
use ratatui::Terminal;
use std::io::{stdout, Result as IoResult};
use std::time::{Duration, Instant};

pub async fn run(mut app: App) -> Result<()> {
    setup_terminal()?;
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)?;

    let res = ui_loop(&mut terminal, &mut app).await;

    // Restore terminal state passed into restore_terminal
    restore_terminal(&mut terminal)?;

    if let Err(e) = res {
        eprintln!("Error: {e}");
    }

    Ok(())
}

fn setup_terminal() -> IoResult<()> {
    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen)?;
    Ok(())
}

// Restore terminal state - needs the terminal instance
fn restore_terminal<B: ratatui::backend::Backend>(terminal: &mut Terminal<B>) -> IoResult<()> {
    disable_raw_mode()?;
    execute!(stdout(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

// Main UI loop - Rewritten for focus management
async fn ui_loop<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    app: &mut App,
) -> IoResult<()> {
    let models = match app.llm.list_models().await {
        Ok(list) if !list.is_empty() => list,
        _ => vec![app.model.clone()],
    };
    let mut menu_selected = models.iter().position(|m| m == &app.model).unwrap_or(0);
    let mut menu_offset = 0;
    let mut quit_pending = false;
    let mut quit_message_timer = None;
    let tick_rate = Duration::from_millis(100);
    let mut last_tick = Instant::now();
    let mut input_buf = String::new();
    let mut last_model_area_height: u16 = 3; // Store the height calculated in the last draw pass

    loop {
        // Drain UI events (from LLM stream)
        while let Ok(UiEvent::Token(role, tok)) = app.rx_ui.try_recv() {
            // Find the last message item and append, or add a new one
            match app.messages.last_mut() {
                Some(ChatLogItem::Message(last_role, ref mut txt)) if *last_role == role => {
                    // Append to existing message of the same role
                    txt.push_str(&tok);
                },
                _ => {
                    // Add new message item
                    app.messages.push(ChatLogItem::Message(role, tok));
                }
            }
        }

        terminal.draw(|f| {
            let size = f.size();
            
            // Define focus border styles
            let border_style = |pane_focus| {
                if app.focus == pane_focus {
                    Style::default().fg(Color::White)
                } else {
                    Style::default().fg(Color::DarkGray)
                }
            };

            // Three-pane vertical layout - Model pane always height 3 (or more when focused)
            let model_area_height = if app.focus == Focus::Model { 11 } else { 3 };
            let vchunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Min(1), // Chat Area
                    Constraint::Length(model_area_height),
                    Constraint::Length(3), // Input Area
                ])
                .split(size);
            let chat_area = vchunks[0];
            let model_area = vchunks[1];
            let input_area = vchunks[2];

            // Update the stored height for use in event handling below
            last_model_area_height = model_area.height;

            // Render Chat Pane
            let lines: Vec<Line> = app.messages.iter().enumerate().map(|(index, item)| {
                let is_selected = app.focus == Focus::Chat && app.selected_message_index == Some(index);
                let line_style = if is_selected {
                    Style::default().bg(Color::DarkGray) // Light box highlight
                } else {
                    Style::default()
                };

                match item {
                    ChatLogItem::Message(role, msg) => {
                        let prefix = match role {
                            Role::User => "You: ",
                            Role::Assistant => "AI: ",
                        };
                        if app.editing && app.selected_message_index == Some(index) {
                            // ## EDITING a REGULAR message ##
                            let diff_segments = compute_diff(msg, &app.edit_buffer); // Diff against current msg
                            let target_buffer_cursor = app.edit_cursor;
                            let mut current_buffer_pos = 0;
                            let mut cursor_inserted = false;
                            let mut rendered_spans = Vec::new();
                            for (segment, is_deletion, is_addition) in diff_segments {
                                let current_segment_len = segment.chars().count();
                                let style = match (is_deletion, is_addition) {
                                    (true, false) => Style::default().fg(Color::Red).add_modifier(Modifier::CROSSED_OUT),
                                    (false, true) => Style::default().fg(Color::Green),
                                    _ => Style::default(),
                                };

                                if is_deletion {
                                    rendered_spans.push(Span::styled(segment.clone(), style));
                                } else {
                                    let segment_start_buffer_pos = current_buffer_pos;
                                    let segment_end_buffer_pos = current_buffer_pos + current_segment_len;

                                    if !cursor_inserted && target_buffer_cursor >= segment_start_buffer_pos && target_buffer_cursor < segment_end_buffer_pos {
                                        let cursor_offset_in_segment = target_buffer_cursor - segment_start_buffer_pos;
                                        let (before_cursor, at_cursor_and_after) = segment.split_at(cursor_offset_in_segment);
                                        
                                        if !before_cursor.is_empty() { rendered_spans.push(Span::styled(before_cursor.to_string(), style)); }
                                        
                                        let cursor_char = at_cursor_and_after.chars().next().unwrap_or(' ');
                                        rendered_spans.push(Span::styled(cursor_char.to_string(), Style::default().fg(Color::Black).bg(Color::Green)));
                                        
                                        if at_cursor_and_after.chars().count() > 1 {
                                            let after_cursor = at_cursor_and_after.chars().skip(1).collect::<String>();
                                            rendered_spans.push(Span::styled(after_cursor, style));
                                        }
                                        cursor_inserted = true;
                                    } else {
                                        rendered_spans.push(Span::styled(segment.clone(), style));
                                    }
                                    current_buffer_pos = segment_end_buffer_pos;
                                }
                            }

                            if !cursor_inserted && target_buffer_cursor == current_buffer_pos {
                                rendered_spans.push(Span::styled(" ", Style::default().bg(Color::Green)));
                                cursor_inserted = true;
                            }

                            let mut final_spans = vec![Span::styled(prefix, Style::default().fg(Color::Yellow))];
                            final_spans.extend(rendered_spans);
                            vec![Line::from(final_spans)]
                        } else {
                            // ## NOT EDITING a REGULAR message ##
                            let mut spans: Vec<Span> = Vec::new();
                            let base_style = if is_selected { line_style } else { Style::default() };
                            spans.push(Span::styled(prefix, base_style.fg(Color::Yellow)));
                            spans.push(Span::styled(msg.clone(), base_style));
                            vec![Line::from(spans)]
                        }
                    },
                    ChatLogItem::EditedMessage(role, msg, add_cnt, del_cnt) => {
                let prefix = match role {
                    Role::User => "You: ",
                    Role::Assistant => "AI: ",
                };
                        if app.editing && app.selected_message_index == Some(index) {
                            // ## EDITING an EDITED message ##
                            let diff_segments = compute_diff(&app.original_text, &app.edit_buffer); // Diff against STORED original
                            let target_buffer_cursor = app.edit_cursor;
                            let mut current_buffer_pos = 0;
                            let mut cursor_inserted = false;
                            let mut rendered_spans = Vec::new();
                            for (segment, is_deletion, is_addition) in diff_segments {
                                let current_segment_len = segment.chars().count();
                                let style = match (is_deletion, is_addition) {
                                    (true, false) => Style::default().fg(Color::Red).add_modifier(Modifier::CROSSED_OUT),
                                    (false, true) => Style::default().fg(Color::Green),
                                    _ => Style::default(),
                                };

                                if is_deletion {
                                    rendered_spans.push(Span::styled(segment.clone(), style));
                                } else {
                                    let segment_start_buffer_pos = current_buffer_pos;
                                    let segment_end_buffer_pos = current_buffer_pos + current_segment_len;

                                    if !cursor_inserted && target_buffer_cursor >= segment_start_buffer_pos && target_buffer_cursor < segment_end_buffer_pos {
                                        let cursor_offset_in_segment = target_buffer_cursor - segment_start_buffer_pos;
                                        let (before_cursor, at_cursor_and_after) = segment.split_at(cursor_offset_in_segment);
                                        
                                        if !before_cursor.is_empty() { rendered_spans.push(Span::styled(before_cursor.to_string(), style)); }
                                        
                                        let cursor_char = at_cursor_and_after.chars().next().unwrap_or(' ');
                                        rendered_spans.push(Span::styled(cursor_char.to_string(), Style::default().fg(Color::Black).bg(Color::Green)));
                                        
                                        if at_cursor_and_after.chars().count() > 1 {
                                            let after_cursor = at_cursor_and_after.chars().skip(1).collect::<String>();
                                            rendered_spans.push(Span::styled(after_cursor, style));
                                        }
                                        cursor_inserted = true;
                                    } else {
                                        rendered_spans.push(Span::styled(segment.clone(), style));
                                    }
                                    current_buffer_pos = segment_end_buffer_pos;
                                }
                            }

                            if !cursor_inserted && target_buffer_cursor == current_buffer_pos {
                                rendered_spans.push(Span::styled(" ", Style::default().bg(Color::Green)));
                                cursor_inserted = true;
                            }

                            let mut final_spans = vec![Span::styled(prefix, Style::default().fg(Color::Yellow))];
                            // Add badge WHEN editing an already edited message
                            if *add_cnt > 0 { final_spans.push(Span::styled(format!(" +{}", add_cnt), Style::default().fg(Color::Green))); }
                            if *del_cnt > 0 { final_spans.push(Span::styled(format!(" -{}", del_cnt), Style::default().fg(Color::Red))); }
                            if *add_cnt > 0 || *del_cnt > 0 { final_spans.push(Span::raw(" ")); }
                            final_spans.extend(rendered_spans);
                            vec![Line::from(final_spans)]
                        } else {
                            // ## NOT EDITING an EDITED message ##
                            let mut spans: Vec<Span> = Vec::new();
                            let base_style = if is_selected { line_style } else { Style::default() };
                            spans.push(Span::styled(prefix, base_style.fg(Color::Yellow)));
                            // Add badge
                            if *add_cnt > 0 { spans.push(Span::styled(format!(" +{}", add_cnt), base_style.fg(Color::Green))); }
                            if *del_cnt > 0 { spans.push(Span::styled(format!(" -{}", del_cnt), base_style.fg(Color::Red))); }
                            if *add_cnt > 0 || *del_cnt > 0 { spans.push(Span::raw(" ")); }
                            spans.push(Span::styled(msg.clone(), base_style));
                            vec![Line::from(spans)]
                        }
                    },
                    ChatLogItem::ModelSwitch(model_name) => {
                        // Build the separator text
                        let sep = format!("--- Switched to New Model: {} ---", model_name);
                        if is_selected {
                            vec![
                                Line::raw(""),
                                Line::from(vec![Span::styled(sep, line_style)]).alignment(ratatui::layout::Alignment::Center),
                                Line::raw(""),
                            ]
                        } else {
                            vec![
                                Line::raw(""),
                                Line::from(vec![Span::styled(
                                    sep,
                                    Style::default().fg(Color::Cyan).add_modifier(Modifier::ITALIC),
                                )]).alignment(ratatui::layout::Alignment::Center),
                                Line::raw(""),
                            ]
                        }
                    },
                    ChatLogItem::InitialModel(msg) => {
                        if is_selected {
                            vec![
                                Line::raw(""),
                                Line::from(vec![Span::styled(
                                    msg.clone(),
                                    line_style,
                                )]).alignment(ratatui::layout::Alignment::Center),
                                Line::raw(""),
                            ]
                        } else {
                            vec![
                                Line::raw(""),
                                Line::from(vec![Span::styled(
                                    msg.clone(),
                                    Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC),
                                )]).alignment(ratatui::layout::Alignment::Center),
                                Line::raw(""),
                            ]
                        }
                    },
                }
            }).flatten() // Flatten the Vec<Vec<Line>> into Vec<Line>
              .collect();
            let chat_height = chat_area.height.saturating_sub(2); // Account for borders
            let num_lines = lines.len();
            let max_scroll = num_lines.saturating_sub(chat_height as usize).try_into().unwrap_or(0);
            app.chat_scroll = app.chat_scroll.min(max_scroll); // Clamp scroll on resize
            
            // Auto-scroll to ensure selected/edited message is visible
            if let Some(selected_idx) = app.selected_message_index {
                // First, find out which line in the flattened lines array contains our selected message
                let mut line_count = 0;
                let mut selected_line = 0;
                
                for (idx, _) in app.messages.iter().enumerate() {
                    if idx == selected_idx {
                        selected_line = line_count;
                        break;
                    }
                    // Add the number of lines this message takes (accounting for ModelSwitch/InitialModel padding)
                    match &app.messages[idx] {
                        ChatLogItem::Message(_, _) | ChatLogItem::EditedMessage(_, _, _, _) => line_count += 1,
                        ChatLogItem::ModelSwitch(_) | ChatLogItem::InitialModel(_) => line_count += 3,
                    }
                }
                
                // Compute visible start and end lines based on current scroll position
                let visible_start = app.chat_scroll as usize;
                let visible_end = visible_start + chat_height as usize;
                
                // Adjust scroll position if selected item is outside visible range
                if selected_line < visible_start {
                    // Scroll up to show selected line (with some context)
                    app.chat_scroll = selected_line.saturating_sub(1) as u16;
                } else if selected_line >= visible_end {
                    // Scroll down to show selected line (with some context)
                    app.chat_scroll = (selected_line - chat_height as usize + 2) as u16;
                    app.chat_scroll = app.chat_scroll.min(max_scroll); // Ensure we don't scroll past the end
                }
            }
            
            let chat = Paragraph::new(lines)
                .block(Block::default().borders(Borders::ALL).title("Chat").border_style(border_style(Focus::Chat)))
                .wrap(Wrap { trim: true })
                .scroll((app.chat_scroll, 0));
            
            // Draw the chat with scrollbar
            f.render_widget(chat.clone(), chat_area);
            
            // Draw scrollbar if needed
            if max_scroll > 0 {
                let scrollbar = Scrollbar::default()
                    .orientation(ScrollbarOrientation::VerticalRight)
                    .begin_symbol(Some("↑"))
                    .end_symbol(Some("↓"));
                
                let scrollbar_state = ScrollbarState::default()
                    .position(app.chat_scroll as usize)
                    .content_length(num_lines);
                
                f.render_stateful_widget(
                    scrollbar,
                    chat_area.inner(&Margin { vertical: 1, horizontal: 0 }),
                    &mut scrollbar_state.clone(),
                );
            }

            // Render Model Pane (Selector or Current Model)
            if app.focus == Focus::Model {
                // Render Full Selector Table when focused
                let max_visible_items = (model_area.height as usize).saturating_sub(4); // Header + borders
                if menu_selected >= menu_offset + max_visible_items {
                    menu_offset = menu_selected.saturating_sub(max_visible_items.saturating_sub(1));
                } else if menu_selected < menu_offset {
                    menu_offset = menu_selected;
                }
                let visible_models = models.iter().skip(menu_offset).take(max_visible_items);
                let rows = visible_models.enumerate().map(|(visible_idx, m)| {
                    let global_idx = visible_idx + menu_offset;
                    let is_selected = global_idx == menu_selected;
                    let is_current = m == &app.model;

                    let display_text = if is_current {
                        format!("{} (current)", m)
                    } else {
                        m.clone()
                    };

                    let style = if is_selected {
                        Style::default().bg(Color::White).fg(Color::Black).add_modifier(Modifier::BOLD)
                    } else if is_current {
                        Style::default().fg(Color::Green) // Highlight current model differently if not selected
                    } else {
                        Style::reset() // Transparent background
                    };
                    Row::new(vec![Cell::from(display_text)]).style(style)
                });
                let header_cells = ["Available Models"].iter().map(|h| Cell::from(*h));
                let header = Row::new(header_cells).style(Style::default().fg(Color::Yellow)).height(1).bottom_margin(1);
                let table = Table::new(rows)
                    .header(header)
                    .block(Block::default().borders(Borders::ALL)
                        .title(format!("Select Model ({}/{}) - Tab/Shift-Tab, ↑/↓, Enter, Esc", menu_selected + 1, models.len()))
                        .border_style(border_style(Focus::Model)))
                    .highlight_style(Style::default().add_modifier(Modifier::BOLD))
                    .highlight_symbol(">> ")
                    .widths(&[Constraint::Percentage(100)]);
                f.render_widget(table, model_area);
            } else {
                // Render Just Current Model when not focused
                let model_text = format!("Model: {}", app.model);
                let paragraph = Paragraph::new(model_text)
                    .block(Block::default().borders(Borders::ALL).title("Model").border_style(border_style(Focus::Model)))
                    .alignment(ratatui::layout::Alignment::Center); // Center the model name
                 f.render_widget(paragraph, model_area);
            }

            // Render Input Pane
            let input_text: Text = input_buf.clone().into();
            let input_title = if quit_pending {
                "Input (Press Esc again to quit)".to_string()
            } else {
                "Input (Enter to send, Tab/Shift-Tab focus)".to_string()
            };
            let input = Paragraph::new(input_text)
                .block(Block::default().borders(Borders::ALL).title(input_title).border_style(border_style(Focus::Input)));
            f.render_widget(input, input_area);

            // Place cursor or hide it based on focus
            match app.focus {
                Focus::Input => {
                    let cursor_x = input_area.x + input_buf.len() as u16 + 1;
                    let cursor_y = input_area.y + 1;
                    f.set_cursor(cursor_x, cursor_y);
                }
                _ => f.set_cursor(0, 0), // Or hide cursor: terminal.hide_cursor()? could be used but draw owns terminal
            }
            
            // Quit confirmation overlay (remains centered)
            if quit_pending {
                let msg = "Press Esc again to quit";
                let w = msg.len() as u16 + 4;
                let h = 3;
                let x = (size.width - w) / 2;
                let y = (size.height - h) / 2;
                let rect = Rect::new(x, y, w, h);
                let block = Block::default().borders(Borders::ALL).title("");
                let text = Paragraph::new(msg).block(block).alignment(ratatui::layout::Alignment::Center);
                f.render_widget(text, rect);
            }
        })?;

        if crossterm::event::poll(tick_rate)? {
            if let CEvent::Key(key) = event::read()? {
                match (app.focus, key.code, key.modifiers) {
                    // --- Specific Esc for Model Pane (must come before global Esc) --- 
                    (Focus::Model, KeyCode::Esc, _) => {
                        app.focus = Focus::Input; // Go back to input field
                        quit_pending = false; // Ensure quit is not triggered
                    }
                    // Edit mode - ESC: cancel edit or Option+Arrow sequences (Meta sends ESC b/f)
                    (Focus::Chat, KeyCode::Esc, _) if app.editing => {
                        // Peek for next key event to catch Option+Left (b) / Option+Right (f)
                        if crossterm::event::poll(Duration::from_millis(50))? {
                            if let CEvent::Key(key2) = event::read()? {
                                match key2.code {
                                    KeyCode::Char('b') => {
                                        app.word_left(); quit_pending = false; continue;
                                    }
                                    KeyCode::Char('f') => {
                                        app.word_right(); quit_pending = false; continue;
                                    }
                                    _ => {}
                                }
                            }
                        }
                        // Otherwise cancel editing
                        app.cancel_edit();
                        quit_pending = false;
                    }
                    // --- Global Quit (now after specific Esc handlers) --- 
                    (_, KeyCode::Esc, _) => {
                        if quit_pending {
                            return Ok(()); // Quit
                        } else {
                            quit_pending = true;
                            quit_message_timer = Some(std::time::Instant::now());
                        }
                    }
                    // --- Tab/Shift-Tab Focus Cycling --- 
                    (_, KeyCode::Tab, KeyModifiers::NONE) => {
                        app.focus = match app.focus {
                            Focus::Chat => Focus::Model,
                            Focus::Model => Focus::Input,
                            Focus::Input => Focus::Chat,
                        };
                        quit_pending = false;
                    }
                    (_, KeyCode::BackTab, KeyModifiers::SHIFT) => {
                        app.focus = match app.focus {
                            Focus::Chat => Focus::Input,
                            Focus::Model => Focus::Chat,
                            Focus::Input => Focus::Model,
                        };
                        quit_pending = false;
                    }
                    // --- Chat Pane --- 
                    (Focus::Chat, KeyCode::Up | KeyCode::Char('k'), _) => {
                        if app.editing {
                            // Do nothing while editing
                        } else {
                            let num_messages = app.messages.len();
                            if num_messages > 0 {
                                let current_index = app.selected_message_index.unwrap_or(0); // Default to top if none selected
                                let next_index = if current_index == 0 {
                                    // Optional: Wrap around to bottom? Or just stay at 0?
                                    // Let's stay at 0 for now.
                                    0
                                } else {
                                    current_index.saturating_sub(1)
                                };
                                app.selected_message_index = Some(next_index);
                                // TODO: Adjust scroll to keep selected item visible
                            }
                        }
                        quit_pending = false;
                    }
                    (Focus::Chat, KeyCode::Down | KeyCode::Char('j'), _) => {
                        if app.editing {
                            // Do nothing while editing
                        } else {
                            let num_messages = app.messages.len();
                            if num_messages > 0 {
                                let current_index = app.selected_message_index.unwrap_or(num_messages -1); // Default to bottom if none selected
                                let next_index = if current_index >= num_messages - 1 {
                                    // Optional: Wrap around to top? Or stay at bottom?
                                    // Let's stay at bottom for now.
                                    num_messages - 1
                                } else {
                                    current_index.saturating_add(1)
                                };
                                app.selected_message_index = Some(next_index);
                                // TODO: Adjust scroll to keep selected item visible
                            }
                        }
                        quit_pending = false;
                    }
                    // Edit mode - Enter key starts editing a selected message
                    (Focus::Chat, KeyCode::Enter, _) if !app.editing => {
                        // Try to start editing the selected message
                        app.start_editing();
                        quit_pending = false;
                    }
                    // Edit mode - Accept changes
                    (Focus::Chat, KeyCode::Enter, _) if app.editing => {
                        app.commit_edit();
                        quit_pending = false;
                    }
                    // Edit mode - Word skip via ALT+Char (Option+Left/Right fallback) meta-b / meta-f
                    (Focus::Chat, KeyCode::Char('b'), KeyModifiers::ALT) if app.editing => { app.word_left(); quit_pending = false; }
                    (Focus::Chat, KeyCode::Char('b'), KeyModifiers::NONE) if app.editing => { app.word_left(); quit_pending = false; }
                    (Focus::Chat, KeyCode::Char('f'), KeyModifiers::ALT) if app.editing => { app.word_right(); quit_pending = false; }
                    (Focus::Chat, KeyCode::Char('f'), KeyModifiers::NONE) if app.editing => { app.word_right(); quit_pending = false; }
                    // Edit mode - Fallback for macOS Option+Left/Right (sends 'b'/'f')
                    (Focus::Chat, KeyCode::Char('b'), KeyModifiers::NONE) if app.editing => { app.word_left(); quit_pending = false; }
                    (Focus::Chat, KeyCode::Char('f'), KeyModifiers::NONE) if app.editing => { app.word_right(); quit_pending = false; }
                    // Edit mode - Word skip (Option/Alt + arrows)
                    (Focus::Chat, KeyCode::Left, modifier) if app.editing && modifier.contains(KeyModifiers::ALT) => { app.word_left(); quit_pending = false; }
                    (Focus::Chat, KeyCode::Right, modifier) if app.editing && modifier.contains(KeyModifiers::ALT) => { app.word_right(); quit_pending = false; }
                    // Edit mode - Jump to start/end (Ctrl+A/E)
                    (Focus::Chat, KeyCode::Char('a'), KeyModifiers::CONTROL) if app.editing => { app.cursor_to_start(); quit_pending = false; }
                    (Focus::Chat, KeyCode::Char('e'), KeyModifiers::CONTROL) if app.editing => { app.cursor_to_end(); quit_pending = false; }
                    // Edit mode - Jump to start/end (Command/Super + arrows)
                    (Focus::Chat, KeyCode::Left, modifier) if app.editing && modifier.contains(KeyModifiers::SUPER) => { app.cursor_to_start(); quit_pending = false; }
                    (Focus::Chat, KeyCode::Right, modifier) if app.editing && modifier.contains(KeyModifiers::SUPER) => { app.cursor_to_end(); quit_pending = false; }
                    // Edit mode - Move cursor one character
                    (Focus::Chat, KeyCode::Left, _) if app.editing => { app.cursor_left(); quit_pending = false; }
                    (Focus::Chat, KeyCode::Right, _) if app.editing => { app.cursor_right(); quit_pending = false; }
                    // Edit mode - Insert and delete characters
                    (Focus::Chat, KeyCode::Char(c), KeyModifiers::NONE) if app.editing => { app.insert_char(c); quit_pending = false; }
                    (Focus::Chat, KeyCode::Backspace, _) if app.editing => { app.delete_char(); quit_pending = false; }
                    // --- Model Selector Pane --- 
                    (Focus::Model, KeyCode::Up | KeyCode::Char('k'), modifier) => {
                        let fast = if modifier == KeyModifiers::SHIFT { 5 } else { 1 };
                        if menu_selected > 0 {
                           menu_selected = menu_selected.saturating_sub(fast);
                        }
                        quit_pending = false;
                    }
                    (Focus::Model, KeyCode::Down | KeyCode::Char('j'), modifier) => {
                        let fast = if modifier == KeyModifiers::SHIFT { 5 } else { 1 };
                        if menu_selected + 1 < models.len() {
                            menu_selected = (menu_selected + fast).min(models.len() - 1);
                        }
                        quit_pending = false;
                    }
                    (Focus::Model, KeyCode::PageUp, _) => {
                        let page_size = last_model_area_height.saturating_sub(4) as usize; // Use height from last draw
                        menu_selected = menu_selected.saturating_sub(page_size);
                        quit_pending = false;
                    }
                    (Focus::Model, KeyCode::PageDown, _) => {
                        let page_size = last_model_area_height.saturating_sub(4) as usize; // Use height from last draw
                        menu_selected = (menu_selected + page_size).min(models.len() - 1);
                       quit_pending = false;
                    }
                    (Focus::Model, KeyCode::Enter, _) => {
                        let new_model = models[menu_selected].clone();
                        let old_model = app.model.clone();
                        // Only add event and update if the model actually changed
                        if new_model != old_model {
                            // Add the switch event *before* changing the model in the app state
                            app.messages.push(ChatLogItem::ModelSwitch(new_model.clone()));
                            app.model = new_model; // Update the app's current model
                        }
                        app.focus = Focus::Input;  // Always move focus to input after selection/confirmation
                        quit_pending = false;
                    }
                    // --- Input Pane --- 
                    (Focus::Input, KeyCode::Char(c), _) => {
                        input_buf.push(c);
                        quit_pending = false;
                    }
                    (Focus::Input, KeyCode::Backspace, _) => {
                        input_buf.pop();
                        quit_pending = false;
                    }
                    (Focus::Input, KeyCode::Enter, _) => {
                        if !input_buf.trim().is_empty() {
                            let msg = input_buf.clone();
                            input_buf.clear();
                            app.messages.push(ChatLogItem::Message(Role::User, msg.clone()));
                            let mut app_clone = app.clone();
                            tokio::spawn(async move {
                                let _ = app_clone.handle_user_msg(msg).await;
                            });
                        }
                        quit_pending = false;
                    }
                    // --- Catch-all for other keys --- 
                    _ => { quit_pending = false; } // Any other key cancels quit prompt
                }
            }
        }

        // Reset quit confirmation after 2 seconds
        if quit_pending {
            if let Some(t0) = quit_message_timer {
                if t0.elapsed() > Duration::from_secs(2) {
                    quit_pending = false;
                    quit_message_timer = None;
                }
            }
        }

        if last_tick.elapsed() >= tick_rate {
            last_tick = Instant::now();
        }
    }

    // Loop only exits via return Ok(()) on Esc, so this is unreachable
}

// Replace the entire compute_diff function with a new implementation
// ... existing code ...
fn compute_diff(original: &str, edited: &str) -> Vec<(String, bool, bool)> {
    if original == edited {
        return vec![(edited.to_string(), false, false)];
    }

    // Tokenize into words + whitespace tokens
    fn tokenize(s: &str) -> Vec<String> {
        let mut tokens = Vec::new();
        let mut current = String::new();
        let mut last_is_space = None;
        for ch in s.chars() {
            let is_space = ch.is_whitespace();
            match last_is_space {
                Some(state) if state == is_space => {
                    current.push(ch);
                }
                Some(_) => {
                    tokens.push(current.clone());
                    current.clear();
                    current.push(ch);
                }
                None => {
                    current.push(ch);
                }
            }
            last_is_space = Some(is_space);
        }
        if !current.is_empty() {
            tokens.push(current);
        }
        tokens
    }

    let a = tokenize(original);
    let b = tokenize(edited);
    let n = a.len();
    let m = b.len();
    let mut dp = vec![vec![0usize; m + 1]; n + 1];

    for i in (0..n).rev() {
        for j in (0..m).rev() {
            if a[i] == b[j] {
                dp[i][j] = dp[i + 1][j + 1] + 1;
            } else {
                dp[i][j] = dp[i + 1][j].max(dp[i][j + 1]);
            }
        }
    }

    let mut i = 0;
    let mut j = 0;
    let mut result = Vec::new();
    while i < n && j < m {
        if a[i] == b[j] {
            result.push((a[i].clone(), false, false));
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            result.push((a[i].clone(), true, false));
            i += 1;
        } else {
            result.push((b[j].clone(), false, true));
            j += 1;
        }
    }
    while i < n {
        result.push((a[i].clone(), true, false));
        i += 1;
    }
    while j < m {
        result.push((b[j].clone(), false, true));
        j += 1;
    }
    result
}

// add helper function near end after compute_diff
fn compute_diff_counts(orig:&str, edit:&str)->(usize,usize){
    let segs=compute_diff(orig,edit);
    let mut add=0;let mut del=0;
    for (s,d,a) in segs{if a{add+=s.chars().count();} if d{del+=s.chars().count();}}
    (add,del)
} 