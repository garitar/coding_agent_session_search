//! Premium theme definitions with world-class, Stripe-level aesthetics.
//!
//! Design principles:
//! - Muted, sophisticated colors that are easy on the eyes
//! - Clear visual hierarchy with accent colors used sparingly
//! - Consistent design language across all elements
//! - High contrast where it matters (text legibility)
//! - Subtle agent differentiation via tinted backgrounds

use ratatui::style::{Color, Modifier, Style};

/// Premium color palette inspired by modern design systems.
/// Uses low-saturation colors for comfort with refined accents for highlights.
pub mod colors {
    use ratatui::style::Color;

    // ═══════════════════════════════════════════════════════════════════════════
    // BASE COLORS - The foundation of the UI
    // ═══════════════════════════════════════════════════════════════════════════

    /// Deep background - primary canvas color
    pub const BG_DEEP: Color = Color::Rgb(26, 27, 38); // #1a1b26

    /// Elevated surface - cards, modals, popups
    pub const BG_SURFACE: Color = Color::Rgb(36, 40, 59); // #24283b

    /// Subtle surface - hover states, selected items
    pub const BG_HIGHLIGHT: Color = Color::Rgb(41, 46, 66); // #292e42

    /// Border color - subtle separators
    pub const BORDER: Color = Color::Rgb(59, 66, 97); // #3b4261

    /// Border accent - focused/active elements
    pub const BORDER_FOCUS: Color = Color::Rgb(125, 145, 200); // #7d91c8

    // ═══════════════════════════════════════════════════════════════════════════
    // TEXT COLORS - Hierarchical text styling
    // ═══════════════════════════════════════════════════════════════════════════

    /// Primary text - headings, important content
    pub const TEXT_PRIMARY: Color = Color::Rgb(192, 202, 245); // #c0caf5

    /// Secondary text - body content
    pub const TEXT_SECONDARY: Color = Color::Rgb(169, 177, 214); // #a9b1d6

    /// Muted text - hints, placeholders, timestamps
    pub const TEXT_MUTED: Color = Color::Rgb(86, 95, 137); // #565f89

    /// Disabled/inactive text
    pub const TEXT_DISABLED: Color = Color::Rgb(68, 75, 106); // #444b6a

    // ═══════════════════════════════════════════════════════════════════════════
    // ACCENT COLORS - Brand and interaction highlights
    // ═══════════════════════════════════════════════════════════════════════════

    /// Primary accent - main actions, links, focus states
    pub const ACCENT_PRIMARY: Color = Color::Rgb(122, 162, 247); // #7aa2f7

    /// Secondary accent - complementary highlights
    pub const ACCENT_SECONDARY: Color = Color::Rgb(187, 154, 247); // #bb9af7

    /// Tertiary accent - subtle highlights
    pub const ACCENT_TERTIARY: Color = Color::Rgb(125, 207, 255); // #7dcfff

    // ═══════════════════════════════════════════════════════════════════════════
    // SEMANTIC COLORS - Role-based coloring (muted versions)
    // ═══════════════════════════════════════════════════════════════════════════

    /// User messages - soft sage green
    pub const ROLE_USER: Color = Color::Rgb(158, 206, 106); // #9ece6a

    /// Agent/Assistant messages - matches primary accent
    pub const ROLE_AGENT: Color = Color::Rgb(122, 162, 247); // #7aa2f7

    /// Tool invocations - warm peach
    pub const ROLE_TOOL: Color = Color::Rgb(255, 158, 100); // #ff9e64

    /// System messages - soft amber
    pub const ROLE_SYSTEM: Color = Color::Rgb(224, 175, 104); // #e0af68

    // ═══════════════════════════════════════════════════════════════════════════
    // STATUS COLORS - Feedback and state indication
    // ═══════════════════════════════════════════════════════════════════════════

    /// Success states
    pub const STATUS_SUCCESS: Color = Color::Rgb(115, 218, 202); // #73daca

    /// Warning states
    pub const STATUS_WARNING: Color = Color::Rgb(224, 175, 104); // #e0af68

    /// Error states
    pub const STATUS_ERROR: Color = Color::Rgb(247, 118, 142); // #f7768e

    /// Info states
    pub const STATUS_INFO: Color = Color::Rgb(125, 207, 255); // #7dcfff

    // ═══════════════════════════════════════════════════════════════════════════
    // AGENT-SPECIFIC TINTS - Subtle background variations
    // ═══════════════════════════════════════════════════════════════════════════

    /// Claude Code - subtle blue tint
    pub const AGENT_CLAUDE_BG: Color = Color::Rgb(28, 31, 48); // blue tint

    /// Codex - subtle green tint
    pub const AGENT_CODEX_BG: Color = Color::Rgb(26, 32, 35); // green tint

    /// Cline - subtle cyan tint
    pub const AGENT_CLINE_BG: Color = Color::Rgb(25, 31, 38); // cyan tint

    /// Gemini - subtle purple tint
    pub const AGENT_GEMINI_BG: Color = Color::Rgb(32, 28, 42); // purple tint

    /// Amp - subtle warm tint
    pub const AGENT_AMP_BG: Color = Color::Rgb(34, 28, 30); // warm tint

    /// OpenCode - neutral
    pub const AGENT_OPENCODE_BG: Color = Color::Rgb(30, 31, 36); // neutral
}

#[derive(Clone, Copy)]
pub struct PaneTheme {
    pub bg: Color,
    pub fg: Color,
    pub accent: Color,
}

#[derive(Clone, Copy)]
pub struct ThemePalette {
    pub accent: Color,
    pub accent_alt: Color,
    pub bg: Color,
    pub fg: Color,
    pub surface: Color,
    pub hint: Color,
    pub border: Color,
    pub user: Color,
    pub agent: Color,
    pub tool: Color,
    pub system: Color,
}

impl ThemePalette {
    /// Light theme - clean, minimal, professional
    pub fn light() -> Self {
        Self {
            accent: Color::Rgb(47, 107, 231),     // Rich blue
            accent_alt: Color::Rgb(124, 93, 198), // Purple
            bg: Color::Rgb(250, 250, 252),        // Off-white
            fg: Color::Rgb(36, 41, 46),           // Near-black
            surface: Color::Rgb(240, 241, 245),   // Light gray
            hint: Color::Rgb(139, 148, 158),      // Medium gray
            border: Color::Rgb(216, 222, 228),    // Border gray
            user: Color::Rgb(45, 138, 72),        // Forest green
            agent: Color::Rgb(47, 107, 231),      // Rich blue
            tool: Color::Rgb(207, 107, 44),       // Warm orange
            system: Color::Rgb(177, 133, 41),     // Amber
        }
    }

    /// Dark theme - premium, refined, easy on the eyes
    pub fn dark() -> Self {
        Self {
            accent: colors::ACCENT_PRIMARY,
            accent_alt: colors::ACCENT_SECONDARY,
            bg: colors::BG_DEEP,
            fg: colors::TEXT_PRIMARY,
            surface: colors::BG_SURFACE,
            hint: colors::TEXT_MUTED,
            border: colors::BORDER,
            user: colors::ROLE_USER,
            agent: colors::ROLE_AGENT,
            tool: colors::ROLE_TOOL,
            system: colors::ROLE_SYSTEM,
        }
    }

    /// Title style - accent colored with bold modifier
    pub fn title(self) -> Style {
        Style::default()
            .fg(self.accent)
            .add_modifier(Modifier::BOLD)
    }

    /// Subtle title style - less prominent headers
    pub fn title_subtle(self) -> Style {
        Style::default().fg(self.fg).add_modifier(Modifier::BOLD)
    }

    /// Hint text style - for secondary/muted information
    pub fn hint_style(self) -> Style {
        Style::default().fg(self.hint)
    }

    /// Border style - for unfocused elements
    pub fn border_style(self) -> Style {
        Style::default().fg(self.border)
    }

    /// Focused border style - for active elements
    pub fn border_focus_style(self) -> Style {
        Style::default().fg(colors::BORDER_FOCUS)
    }

    /// Surface style - for cards, modals, elevated content
    pub fn surface_style(self) -> Style {
        Style::default().bg(self.surface)
    }

    /// Per-agent pane colors - subtle tinted backgrounds with consistent text colors.
    ///
    /// Design philosophy: Instead of jarring, wildly different color schemes per agent,
    /// we use subtle background tints while keeping text colors consistent for legibility.
    /// This creates visual differentiation without sacrificing readability or cohesion.
    pub fn agent_pane(agent: &str) -> PaneTheme {
        let slug = agent.to_lowercase().replace('-', "_");

        let (bg, accent) = match slug.as_str() {
            "claude_code" | "claude" => (colors::AGENT_CLAUDE_BG, colors::ACCENT_PRIMARY),
            "codex" => (colors::AGENT_CODEX_BG, colors::STATUS_SUCCESS),
            "cline" => (colors::AGENT_CLINE_BG, colors::ACCENT_TERTIARY),
            "gemini" | "gemini_cli" => (colors::AGENT_GEMINI_BG, colors::ACCENT_SECONDARY),
            "amp" => (colors::AGENT_AMP_BG, colors::STATUS_ERROR),
            "opencode" => (colors::AGENT_OPENCODE_BG, colors::ROLE_USER),
            _ => (colors::BG_DEEP, colors::ACCENT_PRIMARY),
        };

        PaneTheme {
            bg,
            fg: colors::TEXT_PRIMARY, // Consistent, legible text
            accent,
        }
    }

    /// Get a role-specific style for message rendering
    pub fn role_style(self, role: &str) -> Style {
        let color = match role.to_lowercase().as_str() {
            "user" => self.user,
            "assistant" | "agent" => self.agent,
            "tool" => self.tool,
            "system" => self.system,
            _ => self.hint,
        };
        Style::default().fg(color)
    }

    /// Highlighted text style - for search matches
    pub fn highlight_style(self) -> Style {
        Style::default()
            .fg(colors::BG_DEEP)
            .bg(colors::ACCENT_PRIMARY)
            .add_modifier(Modifier::BOLD)
    }

    /// Selected item style - for list selections
    pub fn selected_style(self) -> Style {
        Style::default()
            .bg(colors::BG_HIGHLIGHT)
            .add_modifier(Modifier::BOLD)
    }

    /// Code block background style
    pub fn code_style(self) -> Style {
        Style::default()
            .bg(colors::BG_SURFACE)
            .fg(colors::TEXT_SECONDARY)
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// STYLE HELPERS - Common style patterns
// ═══════════════════════════════════════════════════════════════════════════════

/// Creates a subtle badge/chip style for filter indicators
pub fn chip_style(palette: ThemePalette) -> Style {
    Style::default()
        .fg(palette.accent_alt)
        .add_modifier(Modifier::BOLD)
}

/// Creates a keyboard shortcut style (for help text)
pub fn kbd_style(palette: ThemePalette) -> Style {
    Style::default()
        .fg(palette.accent)
        .add_modifier(Modifier::BOLD)
}

/// Creates style for score indicators based on magnitude
pub fn score_style(score: f32, palette: ThemePalette) -> Style {
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

    Style::default().fg(color).add_modifier(modifier)
}
