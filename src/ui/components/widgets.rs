//! Premium UI widgets with world-class aesthetics.

use ratatui::layout::Alignment;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

use crate::ui::components::theme::{ThemePalette, colors, kbd_style};
use crate::ui::data::InputMode;

/// Premium search bar widget with refined visual hierarchy.
///
/// Design principles:
/// - Clear visual state indication through subtle border/title changes
/// - Keyboard hints that don't overwhelm the interface
/// - Balanced spacing and typography
pub fn search_bar(
    query: &str,
    palette: ThemePalette,
    input_mode: InputMode,
    mode_label: &str,
    chips: Vec<Span<'static>>,
) -> Paragraph<'static> {
    let in_query_mode = matches!(input_mode, InputMode::Query);

    // Title and border styling based on input mode
    let (title_text, title_style, border_style) = match input_mode {
        InputMode::Query => (
            format!(" Search · {} ", mode_label),
            palette.title(),
            palette.border_style(),
        ),
        InputMode::Agent => (
            " Filter: Agent ".to_string(),
            Style::default()
                .fg(palette.accent_alt)
                .add_modifier(Modifier::BOLD),
            palette.border_focus_style(),
        ),
        InputMode::Workspace => (
            " Filter: Workspace ".to_string(),
            Style::default()
                .fg(palette.accent_alt)
                .add_modifier(Modifier::BOLD),
            palette.border_focus_style(),
        ),
        InputMode::CreatedFrom => (
            " Filter: From Date ".to_string(),
            Style::default()
                .fg(palette.accent_alt)
                .add_modifier(Modifier::BOLD),
            palette.border_focus_style(),
        ),
        InputMode::CreatedTo => (
            " Filter: To Date ".to_string(),
            Style::default()
                .fg(palette.accent_alt)
                .add_modifier(Modifier::BOLD),
            palette.border_focus_style(),
        ),
    };
    let title = Span::styled(title_text, title_style);

    // Query text style
    let query_style = if in_query_mode {
        Style::default().fg(palette.fg)
    } else {
        Style::default().fg(palette.accent_alt)
    };

    // Build the input line with chips and query
    let mut first_line = chips;
    if !first_line.is_empty() {
        first_line.push(Span::raw(" "));
    }

    // Subtle cursor indicator
    let cursor = if in_query_mode { "▎" } else { "│" };
    let prompt = if in_query_mode { "/" } else { "›" };

    first_line.push(Span::styled(
        format!("{} ", prompt),
        Style::default().fg(palette.hint),
    ));
    first_line.push(Span::styled(query.to_string(), query_style));
    first_line.push(Span::styled(
        cursor.to_string(),
        Style::default().fg(palette.accent),
    ));

    // Context-aware hints line - minimal, not overwhelming
    let tips_line = if in_query_mode {
        Line::from(vec![
            Span::styled("F1", kbd_style(palette)),
            Span::styled(" help", Style::default().fg(palette.hint)),
            Span::styled("  ·  ", Style::default().fg(colors::TEXT_DISABLED)),
            Span::styled("F3", Style::default().fg(palette.hint)),
            Span::styled(" agent", Style::default().fg(palette.hint)),
            Span::styled("  F4", Style::default().fg(palette.hint)),
            Span::styled(" workspace", Style::default().fg(palette.hint)),
            Span::styled("  F5", Style::default().fg(palette.hint)),
            Span::styled(" time", Style::default().fg(palette.hint)),
            Span::styled("  ·  ", Style::default().fg(colors::TEXT_DISABLED)),
            Span::styled("Ctrl+Del", Style::default().fg(palette.hint)),
            Span::styled(" clear", Style::default().fg(palette.hint)),
        ])
    } else {
        // Simplified hints when in filter mode
        Line::from(vec![
            Span::styled("Enter", kbd_style(palette)),
            Span::styled(" apply", Style::default().fg(palette.hint)),
            Span::styled("  ·  ", Style::default().fg(colors::TEXT_DISABLED)),
            Span::styled("Esc", Style::default().fg(palette.hint)),
            Span::styled(" cancel", Style::default().fg(palette.hint)),
        ])
    };

    let body = vec![Line::from(first_line), tips_line];

    Paragraph::new(body)
        .block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(border_style),
        )
        .style(Style::default().bg(palette.bg))
        .alignment(Alignment::Left)
}

/// Creates a premium-styled block with consistent theming.
pub fn themed_block<'a>(title: &'a str, palette: ThemePalette, focused: bool) -> Block<'a> {
    let border_style = if focused {
        palette.border_focus_style()
    } else {
        palette.border_style()
    };

    let title_style = if focused {
        palette.title()
    } else {
        palette.title_subtle()
    };

    Block::default()
        .title(Span::styled(format!(" {} ", title), title_style))
        .borders(Borders::ALL)
        .border_style(border_style)
}

/// Creates filter chip spans with premium styling.
pub fn filter_chips(
    agents: &[String],
    workspaces: &[String],
    time_range: Option<&str>,
    palette: ThemePalette,
) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let chip_base = Style::default()
        .fg(palette.accent_alt)
        .add_modifier(Modifier::BOLD);

    if !agents.is_empty() {
        spans.push(Span::styled(format!("[{}]", agents.join(", ")), chip_base));
        spans.push(Span::raw(" "));
    }

    if !workspaces.is_empty() {
        // Truncate long workspace paths for chip display
        let ws_display: Vec<String> = workspaces
            .iter()
            .map(|w| {
                if w.len() > 20 {
                    format!("…{}", &w[w.len().saturating_sub(18)..])
                } else {
                    w.clone()
                }
            })
            .collect();
        spans.push(Span::styled(
            format!("[{}]", ws_display.join(", ")),
            chip_base,
        ));
        spans.push(Span::raw(" "));
    }

    if let Some(time) = time_range {
        spans.push(Span::styled(format!("[{}]", time), chip_base));
        spans.push(Span::raw(" "));
    }

    spans
}

/// Creates a score indicator with visual bars.
pub fn score_indicator(score: f32, palette: ThemePalette) -> Vec<Span<'static>> {
    let normalized = (score / 10.0).clamp(0.0, 1.0);
    let filled = (normalized * 5.0).round() as usize;
    let empty = 5 - filled;

    let color = if score >= 8.0 {
        colors::STATUS_SUCCESS
    } else if score >= 5.0 {
        palette.accent
    } else {
        palette.hint
    };

    let modifier = if score >= 8.0 {
        Modifier::BOLD
    } else if score >= 5.0 {
        Modifier::empty()
    } else {
        Modifier::DIM
    };

    vec![
        Span::styled(
            "●".repeat(filled),
            Style::default().fg(color).add_modifier(modifier),
        ),
        Span::styled(
            "○".repeat(empty),
            Style::default()
                .fg(palette.hint)
                .add_modifier(Modifier::DIM),
        ),
        Span::raw(" "),
        Span::styled(
            format!("{:.1}", score),
            Style::default().fg(color).add_modifier(modifier),
        ),
    ]
}
