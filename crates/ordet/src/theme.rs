use ordet_tui_framework::Color;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ThemePreset {
    Midnight,
    Ordet,
    Stalker,
    Mirror,
    Persona,
    InTheMood,
    Angelopoulos,
    Godfather,
    Jodorowsky,
    SpaceOdyssey2001,
    Edo,
    Greenaway,
    SilverGlobe,
    Parajanov,
    Rohmer,
    Ghibli,
    Amber,
}

impl ThemePreset {
    pub const ALL: [ThemePreset; 13] = [
        ThemePreset::Midnight,
        ThemePreset::Ordet,
        ThemePreset::Stalker,
        ThemePreset::Mirror,
        ThemePreset::Persona,
        ThemePreset::InTheMood,
        ThemePreset::Godfather,
        ThemePreset::Jodorowsky,
        ThemePreset::Edo,
        ThemePreset::Greenaway,
        ThemePreset::SilverGlobe,
        ThemePreset::Rohmer,
        ThemePreset::Amber,
    ];

    pub fn canonical(self) -> Self {
        match self {
            ThemePreset::Angelopoulos
            | ThemePreset::SpaceOdyssey2001
            | ThemePreset::Parajanov
            | ThemePreset::Ghibli => ThemePreset::Midnight,
            preset => preset,
        }
    }

    pub fn next(self) -> Self {
        let index = Self::ALL
            .iter()
            .position(|preset| *preset == self.canonical())
            .unwrap_or(0);
        Self::ALL[(index + 1) % Self::ALL.len()]
    }

    pub fn previous(self) -> Self {
        let index = Self::ALL
            .iter()
            .position(|preset| *preset == self.canonical())
            .unwrap_or(0);
        Self::ALL[(index + Self::ALL.len() - 1) % Self::ALL.len()]
    }

    pub fn name(self) -> &'static str {
        match self.canonical() {
            ThemePreset::Midnight => "Midnight",
            ThemePreset::Ordet => "Ordet",
            ThemePreset::Stalker => "Stalker",
            ThemePreset::Mirror => "Mirror",
            ThemePreset::Persona => "Persona",
            ThemePreset::InTheMood => "In the Mood for Love",
            ThemePreset::Angelopoulos => "Angelopoulos",
            ThemePreset::Godfather => "The Godfather",
            ThemePreset::Jodorowsky => "Jodorowsky",
            ThemePreset::Edo => "Edo",
            ThemePreset::Greenaway => "Greenaway",
            ThemePreset::SilverGlobe => "On the Silver Globe",
            ThemePreset::Rohmer => "Rohmer",
            ThemePreset::Ghibli => "Ghibli",
            ThemePreset::Amber => "Amber",
            ThemePreset::SpaceOdyssey2001 | ThemePreset::Parajanov => "Midnight",
        }
    }

    pub fn palette(self) -> ThemePalette {
        match self.canonical() {
            ThemePreset::Midnight => palette(Palette {
                panel_bg: rgb(7, 11, 20),
                panel_alt_bg: rgb(12, 19, 33),
                header_bg: rgb(10, 16, 29),
                text_fg: rgb(225, 232, 246),
                muted_fg: rgb(134, 147, 176),
                border: rgb(62, 78, 106),
                accent: rgb(120, 179, 255),
                warn: rgb(255, 179, 122),
                cursor_line_bg: rgb(18, 28, 47),
                selection_bg: rgb(29, 47, 76),
                selected_bg: rgb(24, 39, 64),
            }),
            ThemePreset::Ordet => palette(Palette {
                panel_bg: rgb(239, 237, 231),
                panel_alt_bg: rgb(228, 224, 216),
                header_bg: rgb(214, 208, 198),
                text_fg: rgb(31, 34, 39),
                muted_fg: rgb(97, 102, 110),
                border: rgb(162, 160, 154),
                accent: rgb(74, 79, 89),
                warn: rgb(131, 92, 59),
                cursor_line_bg: rgb(221, 216, 207),
                selection_bg: rgb(196, 197, 201),
                selected_bg: rgb(204, 199, 189),
            }),
            ThemePreset::Stalker => palette(Palette {
                panel_bg: rgb(29, 24, 17),
                panel_alt_bg: rgb(39, 33, 25),
                header_bg: rgb(50, 43, 30),
                text_fg: rgb(223, 213, 182),
                muted_fg: rgb(150, 138, 109),
                border: rgb(99, 88, 66),
                accent: rgb(120, 140, 95),
                warn: rgb(182, 135, 82),
                cursor_line_bg: rgb(48, 42, 31),
                selection_bg: rgb(82, 75, 52),
                selected_bg: rgb(63, 59, 42),
            }),
            ThemePreset::Mirror => palette(Palette {
                panel_bg: rgb(27, 29, 38),
                panel_alt_bg: rgb(40, 41, 48),
                header_bg: rgb(62, 56, 46),
                text_fg: rgb(228, 217, 199),
                muted_fg: rgb(155, 144, 132),
                border: rgb(95, 94, 105),
                accent: rgb(175, 137, 88),
                warn: rgb(194, 118, 92),
                cursor_line_bg: rgb(49, 49, 57),
                selection_bg: rgb(79, 72, 68),
                selected_bg: rgb(60, 54, 63),
            }),
            ThemePreset::Persona => palette(Palette {
                panel_bg: rgb(14, 14, 16),
                panel_alt_bg: rgb(24, 24, 27),
                header_bg: rgb(6, 6, 7),
                text_fg: rgb(241, 241, 238),
                muted_fg: rgb(163, 163, 160),
                border: rgb(102, 102, 99),
                accent: rgb(213, 213, 206),
                warn: rgb(148, 51, 59),
                cursor_line_bg: rgb(35, 35, 39),
                selection_bg: rgb(68, 68, 74),
                selected_bg: rgb(41, 41, 45),
            }),
            ThemePreset::InTheMood => palette(Palette {
                panel_bg: rgb(33, 17, 20),
                panel_alt_bg: rgb(53, 24, 29),
                header_bg: rgb(88, 36, 41),
                text_fg: rgb(247, 228, 200),
                muted_fg: rgb(171, 137, 120),
                border: rgb(117, 72, 66),
                accent: rgb(181, 61, 51),
                warn: rgb(222, 171, 74),
                cursor_line_bg: rgb(67, 30, 34),
                selection_bg: rgb(94, 43, 49),
                selected_bg: rgb(108, 48, 55),
            }),
            ThemePreset::Angelopoulos => palette(Palette {
                panel_bg: rgb(40, 48, 42),
                panel_alt_bg: rgb(58, 69, 62),
                header_bg: rgb(90, 108, 97),
                text_fg: rgb(227, 231, 223),
                muted_fg: rgb(158, 167, 159),
                border: rgb(98, 110, 102),
                accent: rgb(141, 168, 148),
                warn: rgb(189, 156, 107),
                cursor_line_bg: rgb(48, 57, 50),
                selection_bg: rgb(86, 101, 91),
                selected_bg: rgb(102, 114, 104),
            }),
            ThemePreset::Godfather => palette(Palette {
                panel_bg: rgb(17, 13, 10),
                panel_alt_bg: rgb(27, 21, 16),
                header_bg: rgb(37, 29, 21),
                text_fg: rgb(232, 212, 178),
                muted_fg: rgb(157, 131, 100),
                border: rgb(99, 74, 52),
                accent: rgb(186, 125, 66),
                warn: rgb(120, 93, 53),
                cursor_line_bg: rgb(38, 29, 22),
                selection_bg: rgb(62, 47, 35),
                selected_bg: rgb(52, 39, 29),
            }),
            ThemePreset::Jodorowsky => palette(Palette {
                panel_bg: rgb(25, 18, 28),
                panel_alt_bg: rgb(40, 27, 44),
                header_bg: rgb(67, 31, 60),
                text_fg: rgb(248, 232, 196),
                muted_fg: rgb(186, 149, 135),
                border: rgb(113, 84, 99),
                accent: rgb(229, 170, 43),
                warn: rgb(71, 170, 216),
                cursor_line_bg: rgb(55, 36, 60),
                selection_bg: rgb(88, 52, 96),
                selected_bg: rgb(82, 39, 80),
            }),
            ThemePreset::SpaceOdyssey2001 => palette(Palette {
                panel_bg: rgb(233, 241, 248),
                panel_alt_bg: rgb(214, 223, 232),
                header_bg: rgb(187, 22, 30),
                text_fg: rgb(23, 26, 31),
                muted_fg: rgb(101, 109, 121),
                border: rgb(186, 194, 206),
                accent: rgb(47, 113, 223),
                warn: rgb(228, 98, 62),
                cursor_line_bg: rgb(230, 235, 240),
                selection_bg: rgb(236, 198, 206),
                selected_bg: rgb(229, 210, 216),
            }),
            ThemePreset::Edo => palette(Palette {
                panel_bg: rgb(43, 28, 18),
                panel_alt_bg: rgb(67, 46, 30),
                header_bg: rgb(97, 70, 32),
                text_fg: rgb(242, 226, 193),
                muted_fg: rgb(189, 162, 118),
                border: rgb(112, 81, 46),
                accent: rgb(208, 166, 71),
                warn: rgb(176, 70, 41),
                cursor_line_bg: rgb(56, 37, 24),
                selection_bg: rgb(106, 74, 37),
                selected_bg: rgb(126, 89, 44),
            }),
            ThemePreset::Greenaway => palette(Palette {
                panel_bg: rgb(22, 10, 10),
                panel_alt_bg: rgb(40, 17, 17),
                header_bg: rgb(69, 18, 18),
                text_fg: rgb(246, 237, 217),
                muted_fg: rgb(177, 149, 132),
                border: rgb(108, 75, 64),
                accent: rgb(152, 27, 34),
                warn: rgb(96, 137, 52),
                cursor_line_bg: rgb(53, 20, 20),
                selection_bg: rgb(83, 31, 31),
                selected_bg: rgb(87, 33, 33),
            }),
            ThemePreset::SilverGlobe => palette(Palette {
                panel_bg: rgb(17, 21, 29),
                panel_alt_bg: rgb(25, 31, 41),
                header_bg: rgb(32, 39, 52),
                text_fg: rgb(219, 225, 233),
                muted_fg: rgb(144, 156, 170),
                border: rgb(87, 100, 115),
                accent: rgb(129, 155, 196),
                warn: rgb(197, 165, 118),
                cursor_line_bg: rgb(36, 43, 56),
                selection_bg: rgb(59, 70, 90),
                selected_bg: rgb(49, 59, 77),
            }),
            ThemePreset::Parajanov => palette(Palette {
                panel_bg: rgb(245, 234, 205),
                panel_alt_bg: rgb(222, 171, 137),
                header_bg: rgb(51, 89, 132),
                text_fg: rgb(41, 29, 26),
                muted_fg: rgb(121, 87, 68),
                border: rgb(181, 132, 85),
                accent: rgb(166, 38, 43),
                warn: rgb(211, 169, 64),
                cursor_line_bg: rgb(231, 216, 188),
                selection_bg: rgb(224, 193, 127),
                selected_bg: rgb(205, 141, 110),
            }),
            // Rohmer is keyed to his summer films: airy daylight, natural light,
            // pale sea/sky blues, washed greens, and a small coral note.
            ThemePreset::Rohmer => palette(Palette {
                panel_bg: rgb(252, 248, 241),
                panel_alt_bg: rgb(227, 241, 238),
                header_bg: rgb(170, 214, 233),
                text_fg: rgb(34, 44, 49),
                muted_fg: rgb(114, 129, 133),
                border: rgb(182, 196, 195),
                accent: rgb(76, 144, 197),
                warn: rgb(221, 129, 107),
                cursor_line_bg: rgb(243, 248, 245),
                selection_bg: rgb(198, 227, 239),
                selected_bg: rgb(217, 238, 244),
            }),
            // Ghibli leans into hand-painted backgrounds: meadow greens, sky blues,
            // lantern golds, and slightly deeper storybook chrome than Rohmer.
            ThemePreset::Ghibli => palette(Palette {
                panel_bg: rgb(23, 31, 44),
                panel_alt_bg: rgb(31, 49, 58),
                header_bg: rgb(48, 71, 83),
                text_fg: rgb(236, 240, 231),
                muted_fg: rgb(158, 171, 169),
                border: rgb(88, 104, 116),
                accent: rgb(96, 211, 151),
                warn: rgb(216, 171, 87),
                cursor_line_bg: rgb(35, 42, 56),
                selection_bg: rgb(64, 92, 114),
                selected_bg: rgb(58, 85, 95),
            }),
            // Amber mono: the phosphor is #AA5500 (rgb(170, 85, 0)) on a
            // black background, the way an amber CRT looked. Bars use the
            // same #AA5500 with inverted (white) text.
            ThemePreset::Amber => palette(Palette {
                panel_bg: rgb(0, 0, 0),
                panel_alt_bg: rgb(10, 5, 0),
                header_bg: rgb(170, 85, 0),
                text_fg: rgb(170, 85, 0),
                muted_fg: rgb(130, 65, 0),
                border: rgb(170, 85, 0),
                accent: rgb(170, 85, 0),
                warn: rgb(204, 51, 0),
                cursor_line_bg: rgb(25, 12, 0),
                selection_bg: rgb(170, 85, 0),
                selected_bg: rgb(40, 20, 0),
            }),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ThemeSelection {
    Preset {
        preset: ThemePreset,
    },
    Auto,
    System,
    Custom {
        base: ThemePreset,
        accent_hex: String,
    },
}

impl Default for ThemeSelection {
    fn default() -> Self {
        Self::Preset {
            preset: ThemePreset::Midnight,
        }
    }
}

impl ThemeSelection {
    pub fn quick_cycle(&self, context: ThemeContext) -> Self {
        Self::Preset {
            preset: self.base_preset(context).next(),
        }
    }

    pub fn quick_cycle_previous(&self, context: ThemeContext) -> Self {
        Self::Preset {
            preset: self.base_preset(context).previous(),
        }
    }

    pub fn display_name(&self, context: ThemeContext) -> String {
        match self {
            ThemeSelection::Preset { preset } => preset.name().to_string(),
            ThemeSelection::Auto => format!("Auto ({})", self.base_preset(context).name()),
            ThemeSelection::System => format!("System ({})", self.base_preset(context).name()),
            ThemeSelection::Custom { base, accent_hex } => {
                format!("Custom {} {}", base.name(), accent_hex)
            }
        }
    }

    pub fn palette(&self, context: ThemeContext) -> ThemePalette {
        match self {
            ThemeSelection::Preset { preset } => preset.palette(),
            ThemeSelection::Auto | ThemeSelection::System => self.base_preset(context).palette(),
            ThemeSelection::Custom { base, accent_hex } => {
                let accent = parse_hex_color(accent_hex).unwrap_or((120, 179, 255));
                tint_palette(base.palette(), accent)
            }
        }
    }

    pub fn base_preset(&self, context: ThemeContext) -> ThemePreset {
        match self {
            ThemeSelection::Preset { preset } => preset.canonical(),
            ThemeSelection::Auto => {
                if is_daylight(context.hour) {
                    ThemePreset::Ordet
                } else {
                    ThemePreset::Midnight
                }
            }
            ThemeSelection::System => {
                if context.system_is_dark.unwrap_or(false) {
                    ThemePreset::Midnight
                } else {
                    ThemePreset::Ordet
                }
            }
            ThemeSelection::Custom { base, .. } => base.canonical(),
        }
    }

    pub fn sanitize(self) -> Self {
        match self {
            ThemeSelection::Preset { preset } => ThemeSelection::Preset {
                preset: preset.canonical(),
            },
            ThemeSelection::Custom { base, accent_hex } => ThemeSelection::Custom {
                base: base.canonical(),
                accent_hex,
            },
            other => other,
        }
    }

    pub fn custom_accent(&self) -> Option<&str> {
        match self {
            ThemeSelection::Custom { accent_hex, .. } => Some(accent_hex.as_str()),
            _ => None,
        }
    }

    pub fn with_custom_accent(&self, input: &str, context: ThemeContext) -> Result<Self, String> {
        let accent_hex = normalize_hex_color(input)?;
        Ok(Self::Custom {
            base: self.base_preset(context),
            accent_hex,
        })
    }

    pub fn from_query(input: &str) -> Option<Self> {
        let normalized = normalize_theme_query(input);
        if normalized.is_empty() {
            return None;
        }

        match normalized.as_str() {
            "auto" => Some(Self::Auto),
            "system" => Some(Self::System),
            "midnight" => Some(Self::Preset {
                preset: ThemePreset::Midnight,
            }),
            "ordet" => Some(Self::Preset {
                preset: ThemePreset::Ordet,
            }),
            "stalker" => Some(Self::Preset {
                preset: ThemePreset::Stalker,
            }),
            "mirror" => Some(Self::Preset {
                preset: ThemePreset::Mirror,
            }),
            "persona" => Some(Self::Preset {
                preset: ThemePreset::Persona,
            }),
            "inthemood" | "inthemoodforlove" | "wongkarwai" => Some(Self::Preset {
                preset: ThemePreset::InTheMood,
            }),
            "godfather" | "thegodfather" | "gordonwillis" => Some(Self::Preset {
                preset: ThemePreset::Godfather,
            }),
            "jodorowsky" | "alejandrojodorowsky" | "holymountain" => Some(Self::Preset {
                preset: ThemePreset::Jodorowsky,
            }),
            "edo" | "jidaigeki" | "edoperiod" => Some(Self::Preset {
                preset: ThemePreset::Edo,
            }),
            "greenaway" | "petergreenaway" | "cookthiefwifeherlover" => Some(Self::Preset {
                preset: ThemePreset::Greenaway,
            }),
            "silverglobe" | "onthesilverglobe" | "zulawski" => Some(Self::Preset {
                preset: ThemePreset::SilverGlobe,
            }),
            "rohmer" | "ericrohmer" => Some(Self::Preset {
                preset: ThemePreset::Rohmer,
            }),
            "amber" | "ambermono" | "ambercrt" => Some(Self::Preset {
                preset: ThemePreset::Amber,
            }),
            _ => None,
        }
    }

    pub fn picker_hint() -> &'static str {
        "Theme: midnight, ordet, stalker, mirror, persona, in the mood, godfather, jodorowsky, edo, greenaway, silver globe, rohmer, amber, auto, system"
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ThemeContext {
    pub hour: u8,
    pub system_is_dark: Option<bool>,
}

#[derive(Clone, Copy, Debug)]
pub struct ThemePalette {
    pub header_bg: Color,
    pub header_fg: Color,
    pub panel_bg: Color,
    pub panel_alt_bg: Color,
    pub text_fg: Color,
    pub muted_fg: Color,
    pub border: Color,
    pub accent: Color,
    pub warn: Color,
    pub cursor_line_bg: Color,
    pub selection_bg: Color,
    pub selection_fg: Color,
    pub selected_bg: Color,
}

impl ThemePalette {
    /// Solid opaque background for the top-bar menu dropdown. Always
    /// Menu and modal sheets share one fill so dropdowns, pickers and the
    /// file browser read as the same raised surface over the editor.
    pub fn menu_bg(&self) -> Color {
        self.panel_alt_bg
    }

    /// Ink for the subdued fields of the header/status bar. The bar is one
    /// surface, so its dim text is the theme's own muted ink pulled toward
    /// the bar's foreground.
    /// The panel inks alone cannot do this job: on a bar that is itself
    /// amber, amber muted text would disappear.
    pub fn bar_muted_fg(&self) -> Color {
        blend_color(self.muted_fg, self.header_fg, 0.45)
    }

    /// Ink for an emphasised bar field (`<B>`, work in flight, a met target,
    /// the transient message). When the bar already carries the ink's hue -
    /// black-on-amber needs its amber accents read as black, the bar's own
    /// foreground carries the emphasis instead.
    pub fn bar_emphasis_fg(&self, ink: Color) -> Color {
        if color_distance(self.header_bg, ink) < 150 {
            self.header_fg
        } else {
            ink
        }
    }

    /// Ink for a row painted on `selected_bg` (the settings row under the
    /// selection bar, list highlights). Derived from the fill, so a light
    /// highlight keeps dark ink and a dark one keeps light ink, whatever the
    /// top bar's foreground happens to be.
    pub fn selected_fg(&self) -> Color {
        readable_foreground(self.selected_bg)
    }
}

struct Palette {
    panel_bg: Color,
    panel_alt_bg: Color,
    header_bg: Color,
    text_fg: Color,
    muted_fg: Color,
    border: Color,
    accent: Color,
    warn: Color,
    cursor_line_bg: Color,
    selection_bg: Color,
    selected_bg: Color,
}

fn palette(spec: Palette) -> ThemePalette {
    ThemePalette {
        header_bg: spec.header_bg,
        header_fg: readable_foreground(spec.header_bg),
        panel_bg: spec.panel_bg,
        panel_alt_bg: spec.panel_alt_bg,
        text_fg: spec.text_fg,
        muted_fg: spec.muted_fg,
        border: spec.border,
        accent: spec.accent,
        warn: spec.warn,
        cursor_line_bg: spec.cursor_line_bg,
        selection_bg: spec.selection_bg,
        selection_fg: readable_foreground(spec.selection_bg),
        selected_bg: spec.selected_bg,
    }
}

fn tint_palette(base: ThemePalette, accent: (u8, u8, u8)) -> ThemePalette {
    let accent_color = Color::Rgb(accent.0, accent.1, accent.2);
    ThemePalette {
        accent: accent_color,
        border: blend_color(base.border, accent_color, 0.25),
        selection_bg: blend_color(base.panel_bg, accent_color, 0.34),
        selection_fg: readable_foreground(Color::Rgb(accent.0, accent.1, accent.2)),
        selected_bg: blend_color(base.panel_alt_bg, accent_color, 0.20),
        cursor_line_bg: blend_color(base.cursor_line_bg, accent_color, 0.15),
        ..base
    }
}

fn readable_foreground(background: Color) -> Color {
    let (r, g, b) = color_components(background);
    let luminance = (0.2126 * r as f32) + (0.7152 * g as f32) + (0.0722 * b as f32);
    if luminance > 150.0 {
        Color::Black
    } else {
        Color::White
    }
}

fn blend_color(base: Color, overlay: Color, amount: f32) -> Color {
    let (br, bg, bb) = color_components(base);
    let (or, og, ob) = color_components(overlay);
    let clamped = amount.clamp(0.0, 1.0);
    let mix = |lhs: u8, rhs: u8| -> u8 {
        ((lhs as f32 * (1.0 - clamped)) + (rhs as f32 * clamped)).round() as u8
    };
    Color::Rgb(mix(br, or), mix(bg, og), mix(bb, ob))
}

fn color_distance(lhs: Color, rhs: Color) -> i32 {
    let (lr, lg, lb) = color_components(lhs);
    let (rr, rg, rb) = color_components(rhs);
    (lr as i32 - rr as i32).abs() + (lg as i32 - rg as i32).abs() + (lb as i32 - rb as i32).abs()
}

fn color_components(color: Color) -> (u8, u8, u8) {
    match color {
        Color::Black => (0, 0, 0),
        Color::White => (255, 255, 255),
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Reset => (0, 0, 0),
    }
}

fn normalize_theme_query(input: &str) -> String {
    input
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn is_daylight(hour: u8) -> bool {
    (7..18).contains(&hour)
}

pub fn normalize_hex_color(input: &str) -> Result<String, String> {
    let trimmed = input.trim().trim_start_matches('#');
    let normalized = match trimmed.len() {
        3 => trimmed
            .chars()
            .map(|ch| [ch, ch])
            .collect::<Vec<_>>()
            .into_iter()
            .flatten()
            .collect::<String>(),
        6 => trimmed.to_string(),
        _ => {
            return Err("Use a color tag like #7fb3ff or #abc.".to_string());
        }
    };

    if !normalized.chars().all(|ch| ch.is_ascii_hexdigit()) {
        return Err("Color tags must be hexadecimal, like #7fb3ff.".to_string());
    }

    Ok(format!("#{}", normalized.to_ascii_lowercase()))
}

pub fn parse_hex_color(input: &str) -> Result<(u8, u8, u8), String> {
    let normalized = normalize_hex_color(input)?;
    let hex = normalized.trim_start_matches('#');
    let parse =
        |start| u8::from_str_radix(&hex[start..start + 2], 16).map_err(|error| error.to_string());
    Ok((parse(0)?, parse(2)?, parse(4)?))
}

const fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::Rgb(r, g, b)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use ordet_tui_framework::Color;

    use super::{
        color_components, normalize_hex_color, ThemeContext, ThemePalette, ThemePreset,
        ThemeSelection,
    };

    #[test]
    fn midnight_is_the_default_theme() {
        assert_eq!(
            ThemeSelection::default(),
            ThemeSelection::Preset {
                preset: ThemePreset::Midnight
            }
        );
    }

    #[test]
    fn auto_theme_switches_between_light_and_dark_presets() {
        let day = ThemeSelection::Auto.base_preset(ThemeContext {
            hour: 10,
            system_is_dark: Some(true),
        });
        let night = ThemeSelection::Auto.base_preset(ThemeContext {
            hour: 22,
            system_is_dark: Some(false),
        });

        assert_eq!(day, ThemePreset::Ordet);
        assert_eq!(night, ThemePreset::Midnight);
    }

    #[test]
    fn theme_queries_accept_human_names() {
        assert_eq!(
            ThemeSelection::from_query("The Godfather"),
            Some(ThemeSelection::Preset {
                preset: ThemePreset::Godfather
            })
        );
        assert_eq!(
            ThemeSelection::from_query("system"),
            Some(ThemeSelection::System)
        );
        assert_eq!(
            ThemeSelection::from_query("amber"),
            Some(ThemeSelection::Preset {
                preset: ThemePreset::Amber
            })
        );
        assert_eq!(ThemeSelection::from_query("Angelopoulos"), None);
        assert_eq!(ThemeSelection::from_query("2001"), None);
        assert_eq!(ThemeSelection::from_query("Parajanov"), None);
        assert_eq!(ThemeSelection::from_query("Ghibli"), None);
    }

    #[test]
    fn the_amber_theme_is_the_amber_crt() {
        let amber = ThemePreset::Amber.palette();

        // Bright amber bar, dark ink on it: the row is `readable_foreground`'s
        // job, so black text falls out of the fill.
        assert_eq!(amber.header_fg, Color::White);
        assert_eq!(ThemePreset::Amber.name(), "Amber");
        assert!(ThemePreset::ALL.contains(&ThemePreset::Amber));
        // A marked block is the bar's amber with dark text, the original's
        // reverse-video block.
        assert_eq!(amber.selection_fg, Color::White);

        // Amber accents would vanish on the amber bar, so the bar's own ink
        // carries `<B>`, work in flight and the transient message there.
        assert_eq!(amber.bar_emphasis_fg(amber.accent), amber.header_fg);
        assert_eq!(amber.bar_emphasis_fg(amber.warn), amber.header_fg);
        // On a dark bar the accent keeps its hue, as it always did.
        let midnight = ThemePreset::Midnight.palette();
        assert_eq!(midnight.bar_emphasis_fg(midnight.accent), midnight.accent);
        assert_eq!(midnight.bar_emphasis_fg(midnight.warn), midnight.warn);
    }

    #[test]
    fn every_preset_keeps_its_bar_and_rows_legible() {
        for preset in ThemePreset::ALL {
            let palette = preset.palette();

            // Bar ink against the bar, in both directions.
            assert!(
                color_distance(palette.header_bg, palette.header_fg) > 300,
                "{preset:?} has unreadable header ink"
            );
            assert!(
                color_distance(palette.header_bg, palette.bar_muted_fg()) > 150,
                "{preset:?} has unreadable status fields"
            );
            // A highlighted row's ink is derived from its own fill.
            assert!(
                color_distance(palette.selected_bg, palette.selected_fg()) > 300,
                "{preset:?} has an unreadable highlighted row"
            );
            assert!(
                color_distance(palette.panel_bg, palette.text_fg) > 250,
                "{preset:?} has unreadable text"
            );
            assert!(
                color_distance(palette.panel_bg, palette.accent) > 150,
                "{preset:?} has an unreadable accent"
            );
            assert!(
                color_distance(palette.panel_bg, palette.warn) > 150,
                "{preset:?} has an unreadable warning ink"
            );
        }
    }

    #[test]
    fn hex_colors_are_normalized() {
        assert_eq!(normalize_hex_color("#ABC"), Ok("#aabbcc".to_string()));
        assert_eq!(normalize_hex_color("7fb3ff"), Ok("#7fb3ff".to_string()));
    }

    #[test]
    fn theme_signatures_are_unique() {
        let signatures = ThemePreset::ALL
            .iter()
            .map(|preset| signature(preset.palette()))
            .collect::<HashSet<_>>();

        assert_eq!(signatures.len(), ThemePreset::ALL.len());
    }

    #[test]
    fn edo_and_rohmer_are_visibly_distinct() {
        let edo = ThemePreset::Edo.palette();
        let rohmer = ThemePreset::Rohmer.palette();

        assert!(color_distance(edo.header_bg, rohmer.header_bg) > 80);
        assert!(color_distance(edo.accent, rohmer.accent) > 120);
        assert!(color_distance(edo.panel_bg, rohmer.panel_bg) > 30);
        assert!(color_distance(edo.selection_bg, rohmer.selection_bg) > 40);
    }

    #[test]
    fn bright_film_themes_do_not_share_the_same_room_tone() {
        assert_panel_groups_are_distinct(&[ThemePreset::Edo, ThemePreset::Rohmer]);
    }

    #[test]
    fn legacy_presets_fallback_to_midnight() {
        assert_eq!(ThemePreset::Angelopoulos.canonical(), ThemePreset::Midnight);
        assert_eq!(
            ThemePreset::SpaceOdyssey2001.canonical(),
            ThemePreset::Midnight
        );
        assert_eq!(ThemePreset::Parajanov.canonical(), ThemePreset::Midnight);
        assert_eq!(ThemePreset::Ghibli.canonical(), ThemePreset::Midnight);

        let sanitized = ThemeSelection::Preset {
            preset: ThemePreset::Ghibli,
        }
        .sanitize();
        assert_eq!(
            sanitized,
            ThemeSelection::Preset {
                preset: ThemePreset::Midnight
            }
        );
    }

    fn signature(palette: ThemePalette) -> [(u8, u8, u8); 5] {
        [
            color_components(palette.header_bg),
            color_components(palette.panel_bg),
            color_components(palette.panel_alt_bg),
            color_components(palette.accent),
            color_components(palette.warn),
        ]
    }

    fn color_distance(lhs: Color, rhs: Color) -> i32 {
        let (lr, lg, lb) = color_components(lhs);
        let (rr, rg, rb) = color_components(rhs);
        (lr as i32 - rr as i32).abs()
            + (lg as i32 - rg as i32).abs()
            + (lb as i32 - rb as i32).abs()
    }

    fn assert_panel_groups_are_distinct(group: &[ThemePreset]) {
        for (index, lhs_preset) in group.iter().enumerate() {
            for rhs_preset in group.iter().skip(index + 1) {
                let lhs = lhs_preset.palette();
                let rhs = rhs_preset.palette();

                assert!(
                    color_distance(lhs.panel_bg, rhs.panel_bg) > 25,
                    "{lhs_preset:?} and {rhs_preset:?} have panel backgrounds that are too close"
                );
                assert!(
                    color_distance(lhs.header_bg, rhs.header_bg) > 45,
                    "{lhs_preset:?} and {rhs_preset:?} have header backgrounds that are too close"
                );
                assert!(
                    color_distance(lhs.selection_bg, rhs.selection_bg) > 35,
                    "{lhs_preset:?} and {rhs_preset:?} have selection colors that are too close"
                );
            }
        }
    }
}
