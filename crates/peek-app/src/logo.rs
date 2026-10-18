//! The Peek mark, rendered in code: an eye peeking from a blue tile. One
//! renderer feeds the window icon, the title bar and the About box.

/// Build stamp: version, short commit (with `-dirty` for uncommitted changes), date.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const COMMIT: &str = env!("PEEK_GIT_SHA");
pub const BUILD_DATE: &str = env!("PEEK_BUILD_DATE");
pub const PROFILE: &str = env!("PEEK_PROFILE");

pub fn version_line() -> String {
    format!("Peek {VERSION} ({COMMIT}, {BUILD_DATE}, {PROFILE})")
}

/// Commit clipped for tight spaces: 7 characters plus a dirty marker.
pub fn short_commit() -> String {
    let (sha, dirty) = match COMMIT.strip_suffix("-dirty") {
        Some(s) => (s, true),
        None => (COMMIT, false),
    };
    format!("{}{}", &sha[..sha.len().min(7)], if dirty { "*" } else { "" })
}

pub use crate::mark::rgba;

pub fn icon_data() -> egui::IconData {
    egui::IconData { rgba: rgba(64), width: 64, height: 64 }
}

/// Texture of the mark for drawing in the UI.
pub fn texture(ctx: &egui::Context) -> egui::TextureHandle {
    let size = 128;
    let image = egui::ColorImage::from_rgba_unmultiplied([size, size], &rgba(size as u32));
    ctx.load_texture("peek-logo", image, egui::TextureOptions::LINEAR)
}

#[cfg(test)]
mod tests {
    #[test]
    fn mark_has_transparent_corners_and_white_eye() {
        let px = super::rgba(64);
        assert_eq!(px[3], 0, "corner is transparent");
        let mid = ((32 * 64 + 20) * 4) as usize; // left part of the eye, away from the pupil
        assert!(px[mid] > 230 && px[mid + 3] == 255, "eye white at {:?}", &px[mid..mid + 4]);
        assert!(super::short_commit().len() <= 8);
    }
}
