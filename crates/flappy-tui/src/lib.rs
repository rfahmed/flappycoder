use anyhow::Result;
use crossterm::event::{self, Event as CEvent, KeyCode, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use flappy_core::{App, UiEvent, Role, Focus};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Style, Color, Modifier};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph, Table, Row, Cell, Wrap};
use ratatui::Terminal;
use std::io::{stdout, Result as IoResult};
use std::time::{Duration, Instant};

pub async fn run(mut app: App) -> Result<()> {
    setup_terminal()?;
    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)?;

    let res = ui_loop(&mut terminal, &mut app).await;

    restore_terminal()?;

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

fn restore_terminal() -> IoResult<()> {
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

    loop {
        // Drain UI events (from LLM stream)
        while let Ok(UiEvent::Token(role, tok)) = app.rx_ui.try_recv() {
            if let Some((last_role, ref mut txt)) = app.messages.last_mut() {
                if *last_role == role {
                    txt.push_str(&tok);
                } else {
                    app.messages.push((role, tok));
                }
            } else {
                app.messages.push((role, tok));
            }
            // Scroll chat to bottom when new message arrives
            let num_lines = app.messages.len();
            if num_lines > 0 {
                 app.chat_scroll = (num_lines - 1).try_into().unwrap_or(0);
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

            // Three-pane vertical layout
            let vchunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Min(1), // Chat Area
                    if app.selector_open { Constraint::Length(11) } else { Constraint::Length(0) }, // Model Selector (collapsible)
                    Constraint::Length(3), // Input Area
                ])
                .split(size);
            let chat_area = vchunks[0];
            let model_area = vchunks[1];
            let input_area = vchunks[2];

            // Render Chat Pane
            let lines: Vec<Line> = app.messages.iter().map(|(role, msg)| {
                let prefix = match role {
                    Role::User => "You: ",
                    Role::Assistant => "AI: ",
                };
                Line::from(vec![Span::styled(prefix, Style::default().fg(Color::Yellow)), Span::raw(msg)])
            }).collect();
            let chat = Paragraph::new(lines)
                .block(Block::default().borders(Borders::ALL).title("Chat").border_style(border_style(Focus::Chat)))
                .wrap(Wrap { trim: true })
                .scroll((app.chat_scroll, 0));
            f.render_widget(chat, chat_area);

            // Render Model Selector Pane (if open)
            if app.selector_open {
                let max_visible_items = (model_area.height as usize).saturating_sub(4); // Header + borders
                if menu_selected >= menu_offset + max_visible_items {
                    menu_offset = menu_selected.saturating_sub(max_visible_items.saturating_sub(1));
                } else if menu_selected < menu_offset {
                    menu_offset = menu_selected;
                }
                let visible_models = models.iter().skip(menu_offset).take(max_visible_items);
                let rows = visible_models.enumerate().map(|(visible_idx, m)| {
                    let global_idx = visible_idx + menu_offset;
                    let style = if global_idx == menu_selected {
                        Style::default().bg(Color::White).fg(Color::Black).add_modifier(Modifier::BOLD)
                    } else {
                        Style::reset() // Transparent background
                    };
                    Row::new(vec![Cell::from(m.clone())]).style(style)
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

        // Event handling will be added in the next commit...
        
        // Simple temporary event handling to allow quitting
        if crossterm::event::poll(tick_rate)? {
            if let CEvent::Key(key) = event::read()? {
                 match key.code {
                      KeyCode::Esc => {
                          if quit_pending {
                              return Ok(()); // Quit
                          } else {
                              quit_pending = true;
                              quit_message_timer = Some(std::time::Instant::now());
                          }
                      }
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

    // Should return IoResult based on original code
    Ok(())
} 