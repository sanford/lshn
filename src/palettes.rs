//! Omarchy's themes, bundled so they can be picked anywhere. Each is the
//! theme's `colors.toml`, as Omarchy ships it (MIT: see `themes/LICENSE`);
//! and lshn's own, `hn`, in the same form.

use omarchy_theme::Palette;

macro_rules! themes {
    ($($name:literal),* $(,)?) => {
        const THEMES: &[(&str, &str)] = &[
            $(($name, include_str!(concat!("../themes/", $name, ".toml")))),*
        ];
    };
}

themes!(
    "catppuccin",
    "catppuccin-latte",
    "ethereal",
    "everforest",
    "flexoki-light",
    "gruvbox",
    "hackerman",
    "hn",
    "kanagawa",
    "last-horizon",
    "lumon",
    "lupine",
    "matte-black",
    "miasma",
    "nord",
    "osaka-jade",
    "retro-82",
    "ristretto",
    "rose-pine",
    "solitude",
    "tokyo-night",
    "vantablack",
    "white",
);

/// The theme lshn starts with, unless another's chosen.
pub const DEFAULT: &str = "hn";

/// Whether a theme paints the whole screen its background, rather than
/// leaving the terminal's: all but lshn's own, which keeps the contrast
/// the terminal has.
pub fn paints_background(name: &str) -> bool {
    name != DEFAULT
}

pub fn names() -> impl Iterator<Item = &'static str> {
    THEMES.iter().map(|(name, _)| *name)
}

/// The bundled theme called `name`, by its own spelling.
pub fn find(name: &str) -> Option<&'static str> {
    names().find(|n| *n == name)
}

/// The palette of a bundled theme, from [`find`].
pub fn palette(name: &str) -> Palette {
    let (_, text) = THEMES
        .iter()
        .find(|(n, _)| *n == name)
        .expect("a bundled theme");
    Palette::parse(text).expect("bundled themes parse")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_theme_parses() {
        for name in names() {
            palette(name);
        }
        assert!(!palette("catppuccin-latte").is_dark());
        assert!(palette("tokyo-night").is_dark());
    }
}
