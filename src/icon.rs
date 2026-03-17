use crate::app::{HDMI1, HDMI2};
use ksni::Icon;

#[derive(Clone, Copy)]
struct Color {
    r: u8,
    g: u8,
    b: u8,
    a: u8,
}

impl Color {
    const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }
}

#[derive(Clone, Copy)]
struct IconPalette {
    background: Color,
    screen: Color,
    accent: Color,
}

pub fn monitor_tray_icon(input: Option<&str>, size: i32) -> Icon {
    let size = size.max(16);
    let palette = icon_palette(input);
    let mut data = vec![Color::rgba(0, 0, 0, 0); (size * size) as usize];
    let inset = (size / 12).max(1);
    let radius = (size / 4).max(2);
    let panel_height = size - inset * 2;
    let panel_width = size - inset * 2;

    fill_round_rect(
        &mut data,
        size,
        inset,
        inset,
        panel_width,
        panel_height,
        radius,
        palette.background,
    );

    let screen_margin_x = (size / 5).max(2);
    let screen_top = (size / 4).max(3);
    let screen_width = size - screen_margin_x * 2;
    let screen_height = (size * 6 / 16).max(5);
    let bezel = (size / 16).max(1);

    fill_round_rect(
        &mut data,
        size,
        screen_margin_x,
        screen_top,
        screen_width,
        screen_height,
        (size / 10).max(2),
        Color::rgb(248, 250, 252),
    );
    fill_round_rect(
        &mut data,
        size,
        screen_margin_x + bezel,
        screen_top + bezel,
        screen_width - bezel * 2,
        screen_height - bezel * 2,
        (size / 12).max(1),
        palette.screen,
    );

    let stem_width = (size / 8).max(2);
    let stem_height = (size / 8).max(2);
    let stem_x = (size - stem_width) / 2;
    let stem_y = screen_top + screen_height;
    fill_rect(
        &mut data,
        size,
        stem_x,
        stem_y,
        stem_width,
        stem_height,
        Color::rgb(248, 250, 252),
    );

    let base_width = (size / 3).max(5);
    let base_height = (size / 14).max(1);
    let base_x = (size - base_width) / 2;
    let base_y = stem_y + stem_height;
    fill_round_rect(
        &mut data,
        size,
        base_x,
        base_y,
        base_width,
        base_height + 1,
        (size / 20).max(1),
        Color::rgb(248, 250, 252),
    );

    let badge_size = (size / 3).max(5);
    let badge_x = size - inset - badge_size;
    let badge_y = size - inset - badge_size;
    fill_round_rect(
        &mut data,
        size,
        badge_x,
        badge_y,
        badge_size,
        badge_size,
        (badge_size / 3).max(2),
        palette.accent,
    );

    let digit_color = palette.screen;
    match input {
        Some(HDMI1) => draw_digit_one(&mut data, size, badge_x, badge_y, badge_size, digit_color),
        Some(HDMI2) => draw_digit_two(&mut data, size, badge_x, badge_y, badge_size, digit_color),
        _ => draw_dot(&mut data, size, badge_x, badge_y, badge_size, digit_color),
    }

    let mut argb = Vec::with_capacity((size * size * 4) as usize);
    for pixel in data {
        argb.push(pixel.a);
        argb.push(pixel.r);
        argb.push(pixel.g);
        argb.push(pixel.b);
    }

    Icon {
        width: size,
        height: size,
        data: argb,
    }
}

fn icon_palette(input: Option<&str>) -> IconPalette {
    match input {
        Some(HDMI1) => IconPalette {
            background: Color::rgb(15, 92, 86),
            screen: Color::rgb(10, 33, 38),
            accent: Color::rgb(94, 234, 212),
        },
        Some(HDMI2) => IconPalette {
            background: Color::rgb(148, 76, 22),
            screen: Color::rgb(55, 25, 9),
            accent: Color::rgb(251, 191, 36),
        },
        _ => IconPalette {
            background: Color::rgb(71, 85, 105),
            screen: Color::rgb(15, 23, 42),
            accent: Color::rgb(226, 232, 240),
        },
    }
}

fn fill_rect(
    canvas: &mut [Color],
    canvas_size: i32,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    color: Color,
) {
    let x_start = x.max(0);
    let y_start = y.max(0);
    let x_end = (x + width).min(canvas_size);
    let y_end = (y + height).min(canvas_size);

    for py in y_start..y_end {
        for px in x_start..x_end {
            canvas[(py * canvas_size + px) as usize] = color;
        }
    }
}

fn fill_round_rect(
    canvas: &mut [Color],
    canvas_size: i32,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    radius: i32,
    color: Color,
) {
    if width <= 0 || height <= 0 {
        return;
    }

    let radius = radius.max(0).min(width / 2).min(height / 2);
    let x_end = (x + width).min(canvas_size);
    let y_end = (y + height).min(canvas_size);

    for py in y.max(0)..y_end {
        for px in x.max(0)..x_end {
            let dx = if px < x + radius {
                x + radius - px - 1
            } else if px >= x + width - radius {
                px - (x + width - radius)
            } else {
                0
            };

            let dy = if py < y + radius {
                y + radius - py - 1
            } else if py >= y + height - radius {
                py - (y + height - radius)
            } else {
                0
            };

            if dx * dx + dy * dy <= radius * radius {
                canvas[(py * canvas_size + px) as usize] = color;
            }
        }
    }
}

fn draw_digit_one(
    canvas: &mut [Color],
    canvas_size: i32,
    badge_x: i32,
    badge_y: i32,
    badge_size: i32,
    color: Color,
) {
    let stroke = (badge_size / 6).max(1);
    let center = badge_x + badge_size / 2;
    let top = badge_y + (badge_size / 5);
    let height = badge_size - (badge_size / 3);

    fill_rect(
        canvas,
        canvas_size,
        center - stroke / 2,
        top,
        stroke,
        height,
        color,
    );
    fill_rect(
        canvas,
        canvas_size,
        center - stroke,
        top + stroke / 2,
        stroke,
        stroke,
        color,
    );
}

fn draw_digit_two(
    canvas: &mut [Color],
    canvas_size: i32,
    badge_x: i32,
    badge_y: i32,
    badge_size: i32,
    color: Color,
) {
    let stroke = (badge_size / 6).max(1);
    let left = badge_x + (badge_size / 5);
    let top = badge_y + (badge_size / 5);
    let width = badge_size - (badge_size / 3);
    let middle = badge_y + badge_size / 2;
    let bottom = badge_y + badge_size - (badge_size / 4);

    fill_rect(canvas, canvas_size, left, top, width, stroke, color);
    fill_rect(
        canvas,
        canvas_size,
        left + width - stroke,
        top,
        stroke,
        middle - top,
        color,
    );
    fill_rect(
        canvas,
        canvas_size,
        left,
        middle - stroke / 2,
        width,
        stroke,
        color,
    );
    fill_rect(
        canvas,
        canvas_size,
        left,
        middle,
        stroke,
        bottom - middle,
        color,
    );
    fill_rect(
        canvas,
        canvas_size,
        left,
        bottom - stroke,
        width,
        stroke,
        color,
    );
}

fn draw_dot(
    canvas: &mut [Color],
    canvas_size: i32,
    badge_x: i32,
    badge_y: i32,
    badge_size: i32,
    color: Color,
) {
    let dot_size = (badge_size / 4).max(1);
    let dot_x = badge_x + (badge_size - dot_size) / 2;
    let dot_y = badge_y + (badge_size - dot_size) / 2;
    fill_round_rect(
        canvas,
        canvas_size,
        dot_x,
        dot_y,
        dot_size,
        dot_size,
        (dot_size / 2).max(1),
        color,
    );
}
