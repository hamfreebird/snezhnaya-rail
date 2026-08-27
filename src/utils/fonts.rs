use ab_glyph::FontArc;
use std::fs;
use tracing::debug;

pub fn load_font() -> FontArc {
    let font_paths = [
        "C:/Windows/Fonts/simhei.ttf",
        "C:/Windows/Fonts/msyh.ttc",
        "C:/Windows/Fonts/yahei.ttf",
        "/usr/share/fonts/truetype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        "/System/Library/Fonts/PingFang.ttc",
        "/System/Library/Fonts/STHeiti Light.ttc",
        "static/fonts/msyh.ttf",
    ];
    for path in font_paths.iter() {
        if let Ok(data) = fs::read(path) {
            if let Ok(font) = FontArc::try_from_vec(data) {
                debug!("成功加载字体: {}", path);
                return font;
            }
        }
    }
    panic!("未找到任何可用字体！当前搜索路径: {:?}", font_paths);
}