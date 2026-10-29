//! RGBA colour parsing for VS Code's `#RGB`/`#RGBA`/`#RRGGBB`/`#RRGGBBAA` hex.

/// An 8-bit-per-channel RGBA colour.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Color { r, g, b, a: 255 }
    }

    /// Parse a CSS-style hex colour as VS Code accepts it. Returns `None` for
    /// anything malformed.
    pub fn parse_hex(s: &str) -> Option<Color> {
        let s = s.strip_prefix('#')?;
        let hex = |slice: &str| u8::from_str_radix(slice, 16).ok();
        match s.len() {
            3 => {
                // #RGB -> each nibble doubled
                let r = hex(&s[0..1])?;
                let g = hex(&s[1..2])?;
                let b = hex(&s[2..3])?;
                Some(Color {
                    r: r * 17,
                    g: g * 17,
                    b: b * 17,
                    a: 255,
                })
            }
            4 => {
                let r = hex(&s[0..1])?;
                let g = hex(&s[1..2])?;
                let b = hex(&s[2..3])?;
                let a = hex(&s[3..4])?;
                Some(Color {
                    r: r * 17,
                    g: g * 17,
                    b: b * 17,
                    a: a * 17,
                })
            }
            6 => Some(Color {
                r: hex(&s[0..2])?,
                g: hex(&s[2..4])?,
                b: hex(&s[4..6])?,
                a: 255,
            }),
            8 => Some(Color {
                r: hex(&s[0..2])?,
                g: hex(&s[2..4])?,
                b: hex(&s[4..6])?,
                a: hex(&s[6..8])?,
            }),
            _ => None,
        }
    }

    /// `0xRRGGBB` as used by many UI toolkits (alpha dropped).
    pub fn to_rgb_u32(self) -> u32 {
        ((self.r as u32) << 16) | ((self.g as u32) << 8) | (self.b as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_hex_lengths() {
        assert_eq!(Color::parse_hex("#fff"), Some(Color::rgb(255, 255, 255)));
        assert_eq!(Color::parse_hex("#1e1e1e"), Some(Color::rgb(30, 30, 30)));
        assert_eq!(
            Color::parse_hex("#1e1e1eff"),
            Some(Color { r: 30, g: 30, b: 30, a: 255 })
        );
        assert_eq!(
            Color::parse_hex("#00000080").map(|c| c.a),
            Some(128)
        );
        assert_eq!(Color::parse_hex("1e1e1e"), None); // no '#'
        assert_eq!(Color::parse_hex("#xyz"), None);
    }

    #[test]
    fn to_rgb_u32_packs() {
        assert_eq!(Color::rgb(0x56, 0x9c, 0xd6).to_rgb_u32(), 0x569cd6);
    }
}
