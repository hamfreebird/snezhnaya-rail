// src/utils/fonts.rs
use ab_glyph::FontArc;
use std::fs;
use tracing::debug;

/// 从配置中的字体路径列表依次尝试加载。
pub fn load_font() -> FontArc {
    let paths = &crate::config::get().paths.font_paths;
    for path in paths.iter() {
        if let Ok(data) = fs::read(path) {
            if let Ok(font) = FontArc::try_from_vec(data) {
                debug!("成功加载字体: {}", path);
                return font;
            }
        }
    }
    panic!("未找到任何可用字体！当前搜索路径: {:?}", paths);
}