use crate::models::{Order, Train};
use ab_glyph::PxScale;
use image::{ImageFormat, Rgb, RgbImage};
use imageproc::drawing::draw_text_mut;
use qrcode::QrCode;
use tracing::debug;

pub fn generate_ticket_image(order: &Order, seat_number: &str, train: &Train) -> Vec<u8> {

    let width = 400;
    let height = 220;
    let mut img = RgbImage::new(width, height);

    let top_color = Rgb([50, 90, 170]);
    let bottom_color = Rgb([220, 235, 255]);
    for y in 0..height {
        let t = y as f32 / (height - 1) as f32;
        let r = (top_color[0] as f32 * (1.0 - t) + bottom_color[0] as f32 * t) as u8;
        let g = (top_color[1] as f32 * (1.0 - t) + bottom_color[1] as f32 * t) as u8;
        let b = (top_color[2] as f32 * (1.0 - t) + bottom_color[2] as f32 * t) as u8;
        for x in 0..width {
            img.put_pixel(x, y, Rgb([r, g, b]));
        }
    }

    let font = crate::utils::fonts::load_font();
    let red = Rgb([255, 0, 0]);
    let dark_blue = Rgb([0, 0, 128]);
    let black = Rgb([0, 0, 0]);
    let scale_small = PxScale { x: 12.0, y: 12.0 };
    let scale_mid = PxScale { x: 20.0, y: 20.0 };
    let scale_big = PxScale { x: 36.0, y: 36.0 };
    let scale_big_line = PxScale { x: 72.0, y: 36.0 };
    let scale_mid2 = PxScale { x: 18.0, y: 18.0 };

    draw_text_mut(&mut img, red, 10, 10, scale_small, &font, &format!("订单号: {}", order.id));
    draw_text_mut(&mut img, dark_blue, 280, 10, scale_small, &font, "中央车站售票处");

    let date = &order.travel_date;
    let departure_time = train.departure_times[0].as_deref().unwrap_or("");
    let info_line = format!("{}  {}开                             {}", date, departure_time, seat_number);
    draw_text_mut(&mut img, dark_blue, 10, 35, scale_mid, &font, &info_line);

    draw_text_mut(&mut img, black, 20, 65, scale_big, &font, &format!("{}", order.from_station));
    draw_text_mut(&mut img, black, 140, 65, scale_big_line, &font, "→");
    draw_text_mut(&mut img, black, 220, 65, scale_big, &font, &format!("{}", order.to_station));

    draw_text_mut(&mut img, dark_blue, 10, 120, scale_mid2, &font, &format!("车次: {}", train.base_train_id));

    let price = order.price / order.seat_count;
    draw_text_mut(&mut img, dark_blue, 10, 140, scale_mid2, &font, &format!("票价: {} 摩拉", price));
    draw_text_mut(&mut img, dark_blue, 10, 160, scale_mid2, &font, "限乘当日当次车  中途下车失效");
    draw_text_mut(&mut img, dark_blue, 10, 180, scale_mid2, &font, "唯我皇安娜丝塔夏将带来无垢的黎明!");

    let qr = QrCode::new(order.id.as_bytes()).unwrap();
    let qr_image = qr.render::<image::Luma<u8>>().build();
    let qr_resized = image::imageops::resize(&qr_image, 60, 60, image::imageops::FilterType::Nearest);
    let qr_rgb = image::DynamicImage::ImageLuma8(qr_resized).into_rgb8();
    image::imageops::overlay(&mut img, &qr_rgb, (width - 70) as i64, (height - 70) as i64);

    let mut buffer = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut buffer), ImageFormat::Png).unwrap();
    debug!("生成车票图片，订单: {}", order.id);
    buffer
}