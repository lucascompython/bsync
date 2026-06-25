use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Style, Stylize},
    symbols::border,
    text::{Line, Span},
    widgets::{Block, Clear, List, ListItem, ListState, Padding, Paragraph, Tabs, Widget},
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
        Span::raw(" Room "),
        Span::styled("<r>", Style::new().fg(Color::Blue).bold()),
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
        Tab::Rooms => draw_rooms(frame, app, content),
        Tab::History => draw_history(frame, app, content),
        Tab::Help => draw_help(frame, content),
    }

    if app.dialog.is_some() {
        draw_dialog(frame, app, area);
    }

    // notification in the bottom-right corner
    if let Some(msg) = &app.notification {
        let msg_width = msg.len().min(40) as u16 + 4;
        let notif_area = Rect::new(
            area.x + area.width.saturating_sub(msg_width + 2),
            area.y + area.height.saturating_sub(2),
            msg_width,
            1,
        );
        Paragraph::new(msg.as_str())
            .style(Style::new().fg(Color::Yellow).bg(Color::DarkGray))
            .render(notif_area, frame.buffer_mut());
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
    let first_local = view.rooms.iter().find(|r| r.is_local);

    let [info, warning] = Layout::vertical([Constraint::Min(1), Constraint::Length(5)]).areas(area);

    let info_lines = vec![
        Line::from(vec![
            Span::styled("Peer ID: ", Style::new().bold()),
            Span::raw(&view.peer_id),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("Ticket:  ", Style::new().bold()),
            Span::raw(
                first_local
                    .and_then(|r| r.ticket.as_ref())
                    .map(|s| s.as_str())
                    .unwrap_or("(no local rooms)"),
            ),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("Rooms:   ", Style::new().bold()),
            Span::raw(
                view.rooms
                    .iter()
                    .map(|r| {
                        let badge = if r.is_local { "\u{2605}" } else { "\u{2192}" };
                        format!("{badge} {}", r.name)
                    })
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
        ]),
        Line::from(""),
        Line::from(vec![Span::styled(
            format!(
                "{} connected, {} pending across all rooms",
                view.rooms
                    .iter()
                    .map(|r| r.connected_peers.len())
                    .sum::<usize>(),
                view.rooms
                    .iter()
                    .map(|r| r.pending_peers.len())
                    .sum::<usize>()
            ),
            Style::new().fg(Color::Green),
        )]),
    ];

    let block = Block::bordered()
        .title(" Status ")
        .padding(Padding::horizontal(1));
    Paragraph::new(info_lines)
        .block(block)
        .render(info, frame.buffer_mut());

    let warning_text = "\u{26a0}\u{fe0f}  bsync sends your clipboard to ALL connected peers.\n    Only connect to peers you trust.\n    Connected peers can see: passwords, 2FA codes, API keys, private text.";
    let warning_block = Block::bordered()
        .title(" Security ")
        .border_style(Style::new().fg(Color::Yellow));
    Paragraph::new(warning_text)
        .block(warning_block)
        .render(warning, frame.buffer_mut());
}

fn draw_peers(frame: &mut Frame, app: &App, area: Rect) {
    let view = app.view();

    let [connected_area, pending_area] =
        Layout::vertical([Constraint::Percentage(60), Constraint::Min(1)]).areas(area);

    let all_connected: Vec<(String, String)> = view
        .rooms
        .iter()
        .flat_map(|r| {
            r.connected_peers
                .iter()
                .map(move |p| (p.clone(), r.name.clone()))
        })
        .collect();

    let connected_items: Vec<ListItem> = if all_connected.is_empty() {
        vec![ListItem::new(Span::styled(
            "No connected peers",
            Style::new().fg(Color::DarkGray),
        ))]
    } else {
        all_connected
            .iter()
            .map(|(p, room)| {
                ListItem::new(Line::from(vec![
                    Span::raw(format!("[{room}] ")),
                    Span::styled(p, Style::new().fg(Color::Green)),
                ]))
            })
            .collect()
    };

    let connected_list = List::new(connected_items)
        .block(Block::bordered().title(format!(" Connected ({}) ", all_connected.len())))
        .highlight_style(Style::new().bg(Color::DarkGray));
    frame.render_stateful_widget(connected_list, connected_area, &mut ListState::default());

    let all_pending: Vec<(String, String)> = view
        .rooms
        .iter()
        .flat_map(|r| {
            r.pending_peers
                .iter()
                .map(move |p| (p.clone(), r.name.clone()))
        })
        .collect();

    let pending_items: Vec<ListItem> = if all_pending.is_empty() {
        vec![ListItem::new(Span::styled(
            "No pending peers",
            Style::new().fg(Color::DarkGray),
        ))]
    } else {
        all_pending
            .iter()
            .map(|(p, room)| {
                ListItem::new(Line::from(vec![
                    Span::raw(format!("[{room}] ")),
                    Span::styled(p, Style::new().fg(Color::Yellow)),
                ]))
            })
            .collect()
    };

    let pending_list = List::new(pending_items)
        .block(Block::bordered().title(format!(" Pending ({}) ", all_pending.len())))
        .highlight_style(Style::new().bg(Color::DarkGray));
    frame.render_stateful_widget(pending_list, pending_area, &mut ListState::default());
}

fn draw_rooms(frame: &mut Frame, app: &App, area: Rect) {
    let view = app.view();

    let items: Vec<ListItem> = if view.rooms.is_empty() {
        vec![ListItem::new(Span::styled(
            "No rooms. Press 'n' to create one or 'c' to join via ticket.",
            Style::new().fg(Color::DarkGray),
        ))]
    } else {
        view.rooms
            .iter()
            .map(|r| {
                let badge = if r.is_local {
                    Span::styled("\u{2605} local ", Style::new().fg(Color::Cyan).bold())
                } else {
                    Span::styled("\u{2192} joined", Style::new().fg(Color::Magenta))
                };
                let peer_info = format!(
                    "  {} conn, {} pending",
                    r.connected_peers.len(),
                    r.pending_peers.len()
                );
                ListItem::new(Line::from(vec![
                    Span::styled(format!("{:<16}", r.name), Style::new().bold()),
                    badge,
                    Span::raw(peer_info),
                ]))
            })
            .collect()
    };

    let list = List::new(items)
        .block(Block::bordered().title(format!(" Rooms ({}) ", view.rooms.len())))
        .highlight_style(Style::new().bg(Color::DarkGray));
    let mut state = ListState::default();
    state.select(Some(
        app.rooms_scroll.min(view.rooms.len().saturating_sub(1)),
    ));
    let [list_area, hint_area] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(area);
    frame.render_stateful_widget(list, list_area, &mut state);

    // Hints pinned to the bottom of the tab
    let hints = Line::from(vec![
        Span::styled(" [n]", Style::new().fg(Color::Green).bold()),
        Span::raw(" new  "),
        Span::styled("[d]", Style::new().fg(Color::Red).bold()),
        Span::raw(" delete  "),
        Span::styled("[T]", Style::new().fg(Color::Cyan).bold()),
        Span::raw(" copy ticket  "),
        Span::styled("[\u{2191}\u{2193}]", Style::new().fg(Color::Blue).bold()),
        Span::raw(" scroll"),
    ]);
    Paragraph::new(hints).render(hint_area, frame.buffer_mut());
}

fn draw_history(frame: &mut Frame, app: &App, area: Rect) {
    let view = app.view();

    let items: Vec<ListItem> = if view.history.is_empty() {
        vec![ListItem::new(Span::styled(
            "No history yet",
            Style::new().fg(Color::DarkGray),
        ))]
    } else {
        view.history
            .iter()
            .map(|entry| {
                let arrow = if entry.is_local {
                    "\u{2192}"
                } else {
                    "\u{2190}"
                };
                let origin = if entry.is_local { "you" } else { &entry.origin };
                ListItem::new(Line::from(vec![
                    Span::raw(format!("{arrow} ")),
                    Span::styled(&entry.preview, Style::new().fg(Color::White)),
                    Span::raw(format!("  ({origin})")),
                ]))
            })
            .collect()
    };

    let list = List::new(items)
        .block(Block::bordered().title(" History "))
        .highlight_style(Style::new().bg(Color::DarkGray));
    let mut state = ListState::default();
    state.select(Some(
        app.history_scroll.min(view.history.len().saturating_sub(1)),
    ));
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
            Span::styled("  1-5       ", Style::new().bold().fg(Color::Cyan)),
            Span::raw("Jump to tab directly"),
        ]),
        Line::from(vec![
            Span::styled(
                "  \u{2191}/\u{2193}     ",
                Style::new().bold().fg(Color::Cyan),
            ),
            Span::raw("Scroll history or rooms list"),
        ]),
        Line::from(vec![
            Span::styled("  Enter     ", Style::new().bold().fg(Color::Cyan)),
            Span::raw("Re-copy selected history item to clipboard"),
        ]),
        Line::from(vec![
            Span::styled("  t         ", Style::new().bold().fg(Color::Cyan)),
            Span::raw("Copy your ticket to clipboard (share with peers)"),
        ]),
        Line::from(vec![
            Span::styled("  c         ", Style::new().bold().fg(Color::Cyan)),
            Span::raw("Open connect dialog (enter ticket)"),
        ]),
        Line::from(vec![
            Span::styled("  r         ", Style::new().bold().fg(Color::Cyan)),
            Span::raw("Open room create dialog"),
        ]),
        Line::from(vec![
            Span::styled("  n         ", Style::new().bold().fg(Color::Cyan)),
            Span::raw("On Rooms tab: create new room"),
        ]),
        Line::from(vec![
            Span::styled("  d         ", Style::new().bold().fg(Color::Cyan)),
            Span::raw("On Rooms tab: delete selected room"),
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
            "About Rooms",
            Style::new().bold().underlined(),
        )]),
        Line::from(""),
        Line::from("  bsync is a P2P clipboard sync tool using iroh-gossip."),
        Line::from("  Your clipboard is shared with all connected peers."),
        Line::from("  Rooms provide logical isolation — peers in room 'work'"),
        Line::from("  can't see peers in room 'home' and vice versa."),
        Line::from(""),
        Line::from("  \u{2605} local  = you created this room, you have a ticket"),
        Line::from("  \u{2192} joined = you connected to someone else's room"),
        Line::from(""),
        Line::from(vec![
            Span::styled("  Ticket: ", Style::new().bold()),
            Span::raw("base64-encoded connection info (public, not secret)"),
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
        Dialog::Approval { room, peer_id } => {
            let short_id = &peer_id[..peer_id.len().min(20)];
            (
                " Peer Approval ",
                vec![
                    Line::from(""),
                    Line::from(vec![
                        Span::raw("Peer "),
                        Span::styled(short_id, Style::new().fg(Color::Yellow).bold()),
                        Span::raw(format!(" wants to connect in room '{room}'.")),
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
        Dialog::RoomCreate { input } => (
            " Create Room ",
            vec![
                Line::from(""),
                Line::from("Enter a name for the new room:"),
                Line::from(""),
                Line::from(vec![
                    Span::styled("> ", Style::new().fg(Color::Cyan).bold()),
                    Span::raw(input),
                    Span::styled("_", Style::new().fg(Color::Cyan)),
                ]),
                Line::from(""),
                Line::from(vec![
                    Span::styled("[Enter]", Style::new().fg(Color::Green).bold()),
                    Span::raw(" Create   "),
                    Span::styled("[Esc]", Style::new().fg(Color::DarkGray)),
                    Span::raw(" Cancel"),
                ]),
            ],
            9,
        ),
        Dialog::RoomDelete { room } => (
            " Delete Room ",
            vec![
                Line::from(""),
                Line::from(vec![
                    Span::raw("Delete room '"),
                    Span::styled(room, Style::new().bold().fg(Color::Red)),
                    Span::raw("'?"),
                ]),
                Line::from("All connections in this room will be dropped."),
                Line::from(""),
                Line::from(vec![
                    Span::styled("[y]", Style::new().fg(Color::Red).bold()),
                    Span::raw(" Delete   "),
                    Span::styled("[n]", Style::new().fg(Color::DarkGray)),
                    Span::raw(" Cancel"),
                ]),
            ],
            7,
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
    Clear.render(popup_area, frame.buffer_mut());
    Paragraph::new(lines)
        .block(block)
        .render(popup_area, frame.buffer_mut());
}
