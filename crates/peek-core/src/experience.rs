//! RDP visual experience: what the remote desktop draws and how much it
//! costs on the wire. Mirrors the Experience tab of Microsoft's client.

use serde::{Deserialize, Serialize};

/// What the server should render, plus how pixels travel.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Experience {
    pub wallpaper: bool,
    pub font_smoothing: bool,
    pub desktop_composition: bool,
    /// Show window contents while dragging.
    pub window_drag: bool,
    pub menu_animations: bool,
    pub visual_styles: bool,
    /// Bits per pixel: 32, 24, 16 or 15. RemoteFX needs 32.
    pub color_depth: u8,
    /// Bulk compression of the stream (worth it below LAN speeds).
    pub compression: bool,
}

impl Default for Experience {
    fn default() -> Self {
        ConnectionSpeed::Lan.experience()
    }
}

impl Experience {
    pub const COLOR_DEPTHS: [u8; 4] = [32, 24, 16, 15];

    pub fn color_depth_label(depth: u8) -> &'static str {
        match depth {
            32 => "Highest quality (32-bit)",
            24 => "True color (24-bit)",
            16 => "High color (16-bit)",
            15 => "High color (15-bit)",
            _ => "Other",
        }
    }
}

/// Connection speed presets, as in mstsc. Each fills in an [`Experience`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionSpeed {
    Modem,
    LowSpeedBroadband,
    Satellite,
    HighSpeedBroadband,
    Wan,
    #[default]
    Lan,
    /// Settings chosen one by one.
    Custom,
}

impl ConnectionSpeed {
    pub const PRESETS: [ConnectionSpeed; 6] = [
        ConnectionSpeed::Lan,
        ConnectionSpeed::Wan,
        ConnectionSpeed::HighSpeedBroadband,
        ConnectionSpeed::Satellite,
        ConnectionSpeed::LowSpeedBroadband,
        ConnectionSpeed::Modem,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ConnectionSpeed::Modem => "Modem (56 kbps)",
            ConnectionSpeed::LowSpeedBroadband => "Low-speed broadband (256 kbps – 2 Mbps)",
            ConnectionSpeed::Satellite => "Satellite (2 – 16 Mbps, high latency)",
            ConnectionSpeed::HighSpeedBroadband => "High-speed broadband (2 – 10 Mbps)",
            ConnectionSpeed::Wan => "WAN (10 Mbps or higher, high latency)",
            ConnectionSpeed::Lan => "LAN (10 Mbps or higher)",
            ConnectionSpeed::Custom => "Custom",
        }
    }

    /// The settings a preset stands for. `Custom` returns the LAN set as a
    /// starting point.
    pub fn experience(self) -> Experience {
        let all = Experience {
            wallpaper: true,
            font_smoothing: true,
            desktop_composition: true,
            window_drag: true,
            menu_animations: true,
            visual_styles: true,
            color_depth: 32,
            compression: false,
        };
        let none = Experience {
            wallpaper: false,
            font_smoothing: false,
            desktop_composition: false,
            window_drag: false,
            menu_animations: false,
            visual_styles: false,
            color_depth: 32,
            compression: true,
        };
        match self {
            ConnectionSpeed::Lan | ConnectionSpeed::Custom => all,
            ConnectionSpeed::Wan => Experience {
                compression: true,
                ..all
            },
            ConnectionSpeed::HighSpeedBroadband => Experience {
                font_smoothing: true,
                desktop_composition: true,
                window_drag: true,
                visual_styles: true,
                ..none
            },
            ConnectionSpeed::Satellite => Experience {
                desktop_composition: true,
                visual_styles: true,
                ..none
            },
            ConnectionSpeed::LowSpeedBroadband => Experience {
                visual_styles: true,
                ..none
            },
            ConnectionSpeed::Modem => Experience {
                color_depth: 16,
                ..none
            },
        }
    }

    /// The preset whose settings equal `e`, or `Custom`.
    pub fn matching(e: &Experience) -> ConnectionSpeed {
        Self::PRESETS
            .into_iter()
            .find(|p| p.experience() == *e)
            .unwrap_or(ConnectionSpeed::Custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_are_distinct_and_round_trip() {
        for p in ConnectionSpeed::PRESETS {
            assert_eq!(ConnectionSpeed::matching(&p.experience()), p, "{p:?}");
        }
        let odd = Experience {
            wallpaper: false,
            ..ConnectionSpeed::Lan.experience()
        };
        assert_eq!(ConnectionSpeed::matching(&odd), ConnectionSpeed::Custom);
    }

    #[test]
    fn slower_presets_draw_less() {
        let on = |e: Experience| {
            [
                e.wallpaper,
                e.font_smoothing,
                e.desktop_composition,
                e.window_drag,
                e.menu_animations,
                e.visual_styles,
            ]
            .iter()
            .filter(|b| **b)
            .count()
        };
        let counts: Vec<usize> = ConnectionSpeed::PRESETS
            .iter()
            .map(|p| on(p.experience()))
            .collect();
        assert!(counts.windows(2).all(|w| w[0] >= w[1]), "{counts:?}");
    }
}
