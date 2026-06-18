use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    symbols::border,
    text::{Line, Span},
    widgets::{Block, Clear, List, ListItem, ListState, Padding, Paragraph, Tabs, Widget},
    Frame,
};

use crate::app::{App, Dialog, Tab};

pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();

    let title = Line::from(" bsync ".bold());
    let instructions = Line::from(vec![
        Span::raw(" Tabs "),
        Span::styled("<Tab>", Style::new().fg(Color::Blue).bold()),
        Span::raw(" Scroll "),
        Span::styled("<\u{2191}\u{2193}>", Style::new().fg(Color::Blue).bold()),
        Span::raw(" Ticket "),
        Span::styled("<t>", Style::new().fg(Color::Blue).bold()),
        Span::raw(" Connect "),
        Span::styled("<c>", Style::new().fg(Color::Blue).bold()),
        Span::raw(" Quit "),
        Span::styled("<q>", Style::new().fg(Color::Blue).bold()),
    ]);
    let block = Block::bordered()
        .title(title.centered())
        .title_bottom(instructions.centered())
        .border_set(border::THICK);

    let inner = block.inner(area);
    block.render(area, frame.buffer_mut());

    let [tab_bar, content] =
        Layout::vertical([Constraint::Length(3), Constraint::Min(1)]).areas(inner);

    draw_tabs(frame, app, tab_bar);

    match app.tab {
        Tab::Status => draw_status(frame, app, content),
        Tab::Peers => draw_peers(frame, app, content),
        Tab::History => draw_history(frame, app, content),
        Tab::Help => draw_help(frame, content),
    }

    if app.dialog.is_some() {
        draw_dialog(frame, app, area);
    }
}

fn draw_tabs(frame: &mut Frame, app: &App, area: Rect) {
    let titles: Vec<Line> = Tab::ALL
        .iter()
        .map(|t| Line::from(format!(" {} ", t.title())))
        .collect();
    let tabs = Tabs::new(titles)
        .select(app.tab as usize)
        .highlight_style(Style::new().fg(Color::Black).bg(Color::Cyan).bold());
    frame.render_widget(tabs, area);
}

fn draw_status(frame: &mut Frame, app: &App, area: Rect) {
    let view = app.view();

    let [info, warning] = Layout::vertical([Constraint::Min(1), Constraint::Length(5)]).areas(area);

    let lines = vec![
        Line::from(vec![
            Span::styled("Ticket:  ", Style::new().bold()),
            Span::raw(&view.ticket),
        ]),
        Line::from(vec![
            Span::styled("         ", Style::new().bold()),
            Span::styled(
                "press <t> to copy to clipboard",
                Style::new().fg(Color::DarkGray),
            ),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("Room:    ", Style::new().bold()),
            Span::raw(&view.room),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("Peers:   ", Style::new().bold()),
            Span::styled(
                format!("{} connected", view.connected_peers.len()),
                Style::new().fg(Color::Green),
            ),
            Span::raw(", "),
            Span::styled(
                format!("{} pending", view.pending_peers.len()),
                Style::new().fg(Color::Yellow),
            ),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("Status:  ", Style::new().bold()),
            Span::raw(&view.status),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("Clipboard: ", Style::new().bold()),
            if app.clipboard_enabled {
                Span::styled("enabled", Style::new().fg(Color::Green))
            } else {
                Span::styled(
                    "disabled (--no-clipboard)",
                    Style::new().fg(Color::DarkGray),
                )
            },
        ]),
    ];

    let block = Block::bordered()
        .title(" Status ")
        .padding(Padding::horizontal(1));
    Paragraph::new(lines)
        .block(block)
        .render(info, frame.buffer_mut());

    let warning_text = "\u{26a0}\u{fe0f}  bsync sends your clipboard to ALL connected peers.\n    Only connect to peers you trust.\n    Connected peers can see: passwords, 2FA codes, API keys, private text.";
    let warning_block = Block::bordered()
        .title(" Security ")
        .border_style(Style::new().fg(Color::Yellow));
    Paragraph::new(warning_text)
        .style(Style::new().fg(Color::Yellow))
        .block(warning_block)
        .render(warning, frame.buffer_mut());
}

fn draw_peers(frame: &mut Frame, app: &App, area: Rect) {
    let view = app.view();

    let [connected_area, pending_area] =
        Layout::vertical([Constraint::Percentage(60), Constraint::Min(1)]).areas(area);

    let connected_items: Vec<ListItem> = if view.connected_peers.is_empty() {
        vec![ListItem::new(Span::styled(
            "No connected peers",
            Style::new().fg(Color::DarkGray),
        ))]
    } else {
        view.connected_peers
            .iter()
            .map(|p| {
                ListItem::new(Line::from(vec![
                    Span::styled("\u{25cf} ", Style::new().fg(Color::Green)),
                    Span::raw(p),
                ]))
            })
            .collect()
    };

    let connected_list = List::new(connected_items)
        .block(Block::bordered().title(format!(" Connected ({}) ", view.connected_peers.len())))
        .highlight_style(Style::new().bg(Color::DarkGray));
    frame.render_stateful_widget(connected_list, connected_area, &mut ListState::default());

    let pending_items: Vec<ListItem> = if view.pending_peers.is_empty() {
        vec![ListItem::new(Span::styled(
            "No pending peers",
            Style::new().fg(Color::DarkGray),
        ))]
    } else {
        view.pending_peers
            .iter()
            .map(|p| {
                ListItem::new(Line::from(vec![
                    Span::styled("\u{25cb} ", Style::new().fg(Color::Yellow)),
                    Span::raw(p),
                ]))
            })
            .collect()
    };

    let pending_list = List::new(pending_items)
        .block(Block::bordered().title(format!(" Pending ({}) ", view.pending_peers.len())))
        .highlight_style(Style::new().bg(Color::DarkGray));
    frame.render_stateful_widget(pending_list, pending_area, &mut ListState::default());
}

fn draw_history(frame: &mut Frame, app: &App, area: Rect) {
    let view = app.view();

    let items: Vec<ListItem> = if view.history.is_empty() {
        vec![ListItem::new(Span::styled(
            "No clipboard history yet",
            Style::new().fg(Color::DarkGray),
        ))]
    } else {
        view.history
            .iter()
            .map(|entry| {
                let icon = if entry.is_local {
                    Span::styled("\u{2191} ", Style::new().fg(Color::Cyan)) // up arrow = sent
                } else {
                    Span::styled("\u{2193} ", Style::new().fg(Color::Green)) // down arrow = received
                };
                let origin = Span::styled(
                    format!("[{}] ", &entry.origin[..entry.origin.len().min(12)]),
                    Style::new().fg(Color::DarkGray),
                );
                let preview = Span::raw(&entry.preview);
                ListItem::new(Line::from(vec![icon, origin, preview]))
            })
            .collect()
    };

    let mut state = ListState::default();
    state.select(Some(
        app.history_scroll.min(view.history.len().saturating_sub(1)),
    ));

    let list = List::new(items)
        .block(Block::bordered().title(format!(" Clipboard History ({}) — Enter to re-copy ", view.history.len())))
        .highlight_style(
            Style::new()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        );
    frame.render_stateful_widget(list, area, &mut state);
}

fn draw_help(frame: &mut Frame, area: Rect) {
    let lines = vec![
        Line::from(vec![Span::styled(
            "Keyboard Shortcuts",
            Style::new().bold().underlined(),
        )]),
        Line::from(""),
        Line::from(vec![
            Span::styled("  Tab       ", Style::new().bold().fg(Color::Cyan)),
            Span::raw("Switch to next tab"),
        ]),
        Line::from(vec![
            Span::styled("  Shift+Tab ", Style::new().bold().fg(Color::Cyan)),
            Span::raw("Switch to previous tab"),
        ]),
        Line::from(vec![
            Span::styled("  1-4       ", Style::new().bold().fg(Color::Cyan)),
            Span::raw("Jump to tab directly"),
        ]),
        Line::from(vec![
            Span::styled(
                "  \u{2191}/\u{2193}     ",
                Style::new().bold().fg(Color::Cyan),
            ),
            Span::raw("Scroll history list"),
        ]),
        Line::from(vec![
            Span::styled("  Enter     ", Style::new().bold().fg(Color::Cyan)),
            Span::raw("Re-copy selected history item to clipboard"),
        ]),
        Line::from(vec![
            Span::styled("  t         ", Style::new().bold().fg(Color::Cyan)),
            Span::raw("Copy your ticket to clipboard (share it with peers)"),
        ]),
        Line::from(vec![
            Span::styled("  c         ", Style::new().bold().fg(Color::Cyan)),
            Span::raw("Open connect dialog (enter ticket)"),
        ]),
        Line::from(vec![
            Span::styled("  y/n       ", Style::new().bold().fg(Color::Cyan)),
            Span::raw("Approve/reject pending peer"),
        ]),
        Line::from(vec![
            Span::styled("  Esc       ", Style::new().bold().fg(Color::Cyan)),
            Span::raw("Close dialog"),
        ]),
        Line::from(vec![
            Span::styled("  q         ", Style::new().bold().fg(Color::Cyan)),
            Span::raw("Quit bsync"),
        ]),
        Line::from(""),
        Line::from(vec![Span::styled(
            "About bsync",
            Style::new().bold().underlined(),
        )]),
        Line::from(""),
        Line::from("  bsync is a P2P clipboard sync tool using iroh-gossip."),
        Line::from("  Your clipboard is shared with all connected peers."),
        Line::from("  Use rooms (--room <name>) for logical isolation."),
        Line::from(""),
        Line::from(vec![
            Span::styled("  Ticket: ", Style::new().bold()),
            Span::raw("base64-encoded connection info (public, not secret)"),
        ]),
        Line::from(vec![
            Span::styled("  Room:   ", Style::new().bold()),
            Span::raw("derives gossip topic for logical peer isolation"),
        ]),
    ];

    let block = Block::bordered()
        .title(" Help ")
        .padding(Padding::horizontal(1));
    Paragraph::new(lines)
        .block(block)
        .render(area, frame.buffer_mut());
}

fn draw_dialog(frame: &mut Frame, app: &App, area: Rect) {
    let dialog = app.dialog.as_ref().unwrap();

    let (title, lines, height) = match dialog {
        Dialog::Approval { peer_id } => {
            let short_id = &peer_id[..peer_id.len().min(20)];
            (
                " Peer Approval ",
                vec![
                    Line::from(""),
                    Line::from(vec![
                        Span::raw("Peer "),
                        Span::styled(short_id, Style::new().fg(Color::Yellow).bold()),
                        Span::raw(" wants to connect."),
                    ]),
                    Line::from(""),
                    Line::from("They will be able to see your clipboard."),
                    Line::from(""),
                    Line::from(vec![
                        Span::styled("[y]", Style::new().fg(Color::Green).bold()),
                        Span::raw(" Allow   "),
                        Span::styled("[n]", Style::new().fg(Color::Red).bold()),
                        Span::raw(" Reject   "),
                        Span::styled("[Esc]", Style::new().fg(Color::DarkGray)),
                        Span::raw(" Cancel"),
                    ]),
                ],
                8,
            )
        }
        Dialog::ConnectInput { input } => (
            " Connect to Peer ",
            vec![
                Line::from(""),
                Line::from("Enter the ticket from the peer you want to connect to:"),
                Line::from(""),
                Line::from(vec![
                    Span::styled("> ", Style::new().fg(Color::Cyan).bold()),
                    Span::raw(input),
                    Span::styled("_", Style::new().fg(Color::Cyan)),
                ]),
                Line::from(""),
                Line::from(vec![
                    Span::styled("[Enter]", Style::new().fg(Color::Green).bold()),
                    Span::raw(" Connect   "),
                    Span::styled("[Esc]", Style::new().fg(Color::DarkGray)),
                    Span::raw(" Cancel"),
                ]),
            ],
            9,
        ),
        Dialog::Error { message } => (
            " Error ",
            vec![
                Line::from(""),
                Line::from(Span::styled(message, Style::new().fg(Color::Red))),
                Line::from(""),
                Line::from(vec![
                    Span::styled("[Enter]", Style::new().fg(Color::Yellow).bold()),
                    Span::raw(" Dismiss"),
                ]),
            ],
            6,
        ),
        Dialog::Info { message } => (
            " Info ",
            vec![
                Line::from(""),
                Line::from(message.as_str()),
                Line::from(""),
                Line::from(vec![
                    Span::styled("[Enter]", Style::new().fg(Color::Cyan).bold()),
                    Span::raw(" Dismiss"),
                ]),
            ],
            6,
        ),
    };

    let width = 60.min(area.width.saturating_sub(4));
    let popup_area = Rect::new(
        area.x + (area.width.saturating_sub(width)) / 2,
        area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    );

    let block = Block::bordered()
        .title(title)
        .border_style(Style::new().fg(Color::Cyan));
    let inner = block.inner(popup_area);

    frame.render_widget(Clear, popup_area);
    block.render(popup_area, frame.buffer_mut());
    Paragraph::new(lines).render(inner, frame.buffer_mut());
}
