//! Font database shared by every SVG render.
//!
//! The design fonts (Saira Medium and Source Sans Pro Regular) are embedded
//! so the greeter still looks right on systems where they are not installed.
//! Source Sans Pro contains no CJK glyphs, so a subset of Noto Sans CJK SC
//! (Regular) is embedded as the same-style fallback for the Chinese copy.

use std::sync::{Arc, OnceLock};

use fontdb::Database;

static DATABASE: OnceLock<Arc<Database>> = OnceLock::new();

pub fn database() -> Arc<Database> {
    DATABASE
        .get_or_init(|| {
            let mut db = Database::new();
            // Load the embedded design fonts before system fonts so the
            // intended metrics win even if the system has other versions.
            db.load_font_data(include_bytes!("../assets/fonts/Saira-Medium.ttf").to_vec());
            db.load_font_data(include_bytes!("../assets/fonts/SourceSansPro-Regular.otf").to_vec());
            db.load_font_data(
                include_bytes!("../assets/fonts/NotoSansCJKsc-Regular-subset.otf").to_vec(),
            );
            // The env var is a development aid to prove that the embedded
            // fonts are sufficient on a system without any of them installed.
            if std::env::var_os("PHIGRAUTH_EMBEDDED_FONTS_ONLY").is_none() {
                db.load_system_fonts();
            }
            Arc::new(db)
        })
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fontdb::{Family, Query, Stretch, Style, Weight};

    #[test]
    fn embedded_design_fonts_are_available() {
        let mut db = Database::new();
        db.load_font_data(include_bytes!("../assets/fonts/Saira-Medium.ttf").to_vec());
        db.load_font_data(include_bytes!("../assets/fonts/SourceSansPro-Regular.otf").to_vec());
        db.load_font_data(
            include_bytes!("../assets/fonts/NotoSansCJKsc-Regular-subset.otf").to_vec(),
        );

        let saria = [Family::Name("Saira")];
        let q = Query {
            families: &saria,
            weight: Weight(500),
            stretch: Stretch::Normal,
            style: Style::Normal,
        };
        db.query(&q).expect("embedded Saira Medium");

        let source_sans = [Family::Name("Source Sans Pro")];
        let q = Query {
            families: &source_sans,
            weight: Weight(400),
            stretch: Stretch::Normal,
            style: Style::Normal,
        };
        db.query(&q).expect("embedded Source Sans Pro");

        let cjk = [Family::Name("Noto Sans CJK SC")];
        let q = Query {
            families: &cjk,
            weight: Weight(400),
            stretch: Stretch::Normal,
            style: Style::Normal,
        };
        db.query(&q).expect("embedded CJK fallback");
    }
}
