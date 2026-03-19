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
    shell: Color,
    shell_highlight: Color,
    screen: Color,
    screen_detail: Color,
    accent: Color,
    accent_muted: Color,
    badge_text: Color,
}

#[derive(Clone, Copy)]
struct Rect {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}

struct Canvas {
    size: i32,
    pixels: Vec<Color>,
}

impl Canvas {
    fn new(size: i32) -> Self {
        Self {
            size,
            pixels: vec![Color::rgba(0, 0, 0, 0); (size * size) as usize],
        }
    }

    fn into_argb(self) -> Vec<u8> {
        let mut argb = Vec::with_capacity((self.size * self.size * 4) as usize);
        for pixel in self.pixels {
            argb.push(pixel.a);
            argb.push(pixel.r);
            argb.push(pixel.g);
            argb.push(pixel.b);
        }
        argb
    }

    fn fill_round_rect(&mut self, rect: Rect, radius: i32, color: Color) {
        if rect.width <= 0 || rect.height <= 0 {
            return;
        }

        let radius = radius.max(0).min(rect.width / 2).min(rect.height / 2);
        let x_end = (rect.x + rect.width).min(self.size);
        let y_end = (rect.y + rect.height).min(self.size);

        for py in rect.y.max(0)..y_end {
            for px in rect.x.max(0)..x_end {
                let dx = if px < rect.x + radius {
                    rect.x + radius - px - 1
                } else if px >= rect.x + rect.width - radius {
                    px - (rect.x + rect.width - radius)
                } else {
                    0
                };

                let dy = if py < rect.y + radius {
                    rect.y + radius - py - 1
                } else if py >= rect.y + rect.height - radius {
                    py - (rect.y + rect.height - radius)
                } else {
                    0
                };

                if dx * dx + dy * dy <= radius * radius {
                    self.pixels[(py * self.size + px) as usize] = color;
                }
            }
        }
    }
}

pub fn monitor_tray_icon(input: Option<&str>, size: i32) -> Icon {
    let size = size.max(16);
    let palette = icon_palette(input);
    let mut canvas = Canvas::new(size);

    let shadow_offset = (size / 18).max(1);
    let shell_x = (size / 8).max(1);
    let shell_y = (size / 6).max(2);
    let shell_width = size - shell_x * 2;
    let shell_height = (size * 9 / 16).max(8);
    let shell_radius = (size / 5).max(2);

    canvas.fill_round_rect(
        Rect {
            x: shell_x,
            y: shell_y + shadow_offset,
            width: shell_width,
            height: shell_height,
        },
        shell_radius,
        Color::rgba(15, 23, 42, 70),
    );
    canvas.fill_round_rect(
        Rect {
            x: shell_x,
            y: shell_y,
            width: shell_width,
            height: shell_height,
        },
        shell_radius,
        palette.shell,
    );

    let screen_inset = (size / 14).max(1);
    let screen_x = shell_x + screen_inset;
    let screen_y = shell_y + screen_inset;
    let screen_width = shell_width - screen_inset * 2;
    let screen_height = shell_height - screen_inset * 2;

    canvas.fill_round_rect(
        Rect {
            x: screen_x,
            y: screen_y,
            width: screen_width,
            height: screen_height,
        },
        (size / 7).max(2),
        palette.screen,
    );

    canvas.fill_round_rect(
        Rect {
            x: screen_x + screen_inset,
            y: screen_y + screen_inset,
            width: (screen_width * 2 / 3).max(3),
            height: (size / 18).max(1),
        },
        1,
        palette.shell_highlight,
    );

    draw_screen_pattern(
        &mut canvas,
        Rect {
            x: screen_x,
            y: screen_y,
            width: screen_width,
            height: screen_height,
        },
        input,
        palette,
    );

    let stem_width = (size / 10).max(2);
    let stem_height = (size / 7).max(2);
    let stem_x = (size - stem_width) / 2;
    let stem_y = shell_y + shell_height - screen_inset / 2;
    canvas.fill_round_rect(
        Rect {
            x: stem_x,
            y: stem_y,
            width: stem_width,
            height: stem_height,
        },
        (stem_width / 2).max(1),
        palette.shell,
    );

    let base_width = (size * 7 / 20).max(6);
    let base_height = (size / 16).max(1);
    let base_x = (size - base_width) / 2;
    let base_y = stem_y + stem_height - 1;
    canvas.fill_round_rect(
        Rect {
            x: base_x,
            y: base_y,
            width: base_width,
            height: base_height + 1,
        },
        (base_height + 1).max(1),
        palette.shell_highlight,
    );

    let badge_size = (size * 5 / 16).max(6);
    let badge_x = shell_x + shell_width - badge_size + (size / 30).max(0);
    let badge_y = shell_y + shell_height - badge_size / 2;
    draw_badge(&mut canvas, badge_x, badge_y, badge_size, input, palette);

    Icon {
        width: size,
        height: size,
        data: canvas.into_argb(),
    }
}

fn icon_palette(input: Option<&str>) -> IconPalette {
    match input {
        Some(HDMI1) => IconPalette {
            shell: Color::rgb(16, 24, 40),
            shell_highlight: Color::rgb(71, 85, 105),
            screen: Color::rgb(11, 42, 54),
            screen_detail: Color::rgb(18, 74, 90),
            accent: Color::rgb(52, 211, 153),
            accent_muted: Color::rgb(110, 231, 183),
            badge_text: Color::rgb(4, 26, 30),
        },
        Some(HDMI2) => IconPalette {
            shell: Color::rgb(19, 24, 34),
            shell_highlight: Color::rgb(100, 116, 139),
            screen: Color::rgb(61, 27, 43),
            screen_detail: Color::rgb(104, 42, 68),
            accent: Color::rgb(251, 146, 60),
            accent_muted: Color::rgb(253, 186, 116),
            badge_text: Color::rgb(58, 24, 8),
        },
        _ => IconPalette {
            shell: Color::rgb(30, 41, 59),
            shell_highlight: Color::rgb(148, 163, 184),
            screen: Color::rgb(30, 41, 59),
            screen_detail: Color::rgb(51, 65, 85),
            accent: Color::rgb(226, 232, 240),
            accent_muted: Color::rgb(203, 213, 225),
            badge_text: Color::rgb(15, 23, 42),
        },
    }
}

fn draw_screen_pattern(
    canvas: &mut Canvas,
    screen: Rect,
    input: Option<&str>,
    palette: IconPalette,
) {
    let module_x = screen.x + (screen.width / 6).max(1);
    let module_y = screen.y + (screen.height / 4).max(1);
    let module_width = (screen.width / 4).max(3);
    let module_height = (screen.height / 4).max(3);

    canvas.fill_round_rect(
        Rect {
            x: module_x,
            y: module_y,
            width: module_width,
            height: module_height,
        },
        (module_height / 3).max(1),
        palette.screen_detail,
    );

    let rail_height = (screen.height / 8).max(1);
    let rail_width = screen.width - (screen.width / 4).max(3);
    let rail_x = screen.x + (screen.width / 7).max(1);
    let rail_y = screen.y + screen.height - rail_height - (screen.height / 5).max(1);

    match input {
        Some(HDMI2) => {
            let gap = (rail_height / 2).max(1);
            let top_width = (rail_width * 3 / 4).max(3);
            let bottom_width = rail_width;

            canvas.fill_round_rect(
                Rect {
                    x: rail_x,
                    y: rail_y - rail_height - gap,
                    width: top_width,
                    height: rail_height,
                },
                rail_height,
                palette.accent_muted,
            );
            canvas.fill_round_rect(
                Rect {
                    x: rail_x,
                    y: rail_y,
                    width: bottom_width,
                    height: rail_height,
                },
                rail_height,
                palette.accent,
            );
        }
        Some(HDMI1) => {
            canvas.fill_round_rect(
                Rect {
                    x: rail_x,
                    y: rail_y,
                    width: rail_width,
                    height: rail_height,
                },
                rail_height,
                palette.accent,
            );
        }
        _ => {
            canvas.fill_round_rect(
                Rect {
                    x: rail_x,
                    y: rail_y,
                    width: rail_width,
                    height: rail_height,
                },
                rail_height,
                palette.accent_muted,
            );
        }
    }
}

fn draw_badge(
    canvas: &mut Canvas,
    badge_x: i32,
    badge_y: i32,
    badge_size: i32,
    input: Option<&str>,
    palette: IconPalette,
) {
    let shadow_offset = (badge_size / 10).max(1);
    canvas.fill_round_rect(
        Rect {
            x: badge_x,
            y: badge_y + shadow_offset,
            width: badge_size,
            height: badge_size,
        },
        badge_size / 2,
        Color::rgba(15, 23, 42, 70),
    );
    canvas.fill_round_rect(
        Rect {
            x: badge_x,
            y: badge_y,
            width: badge_size,
            height: badge_size,
        },
        badge_size / 2,
        Color::rgb(241, 245, 249),
    );

    let inset = (badge_size / 8).max(1);
    canvas.fill_round_rect(
        Rect {
            x: badge_x + inset,
            y: badge_y + inset,
            width: badge_size - inset * 2,
            height: badge_size - inset * 2,
        },
        (badge_size - inset * 2) / 2,
        palette.accent,
    );

    match input {
        Some(HDMI1) => draw_digit_one(
            canvas,
            badge_x + inset,
            badge_y + inset,
            badge_size - inset * 2,
            palette.badge_text,
        ),
        Some(HDMI2) => draw_digit_two(
            canvas,
            badge_x + inset,
            badge_y + inset,
            badge_size - inset * 2,
            palette.badge_text,
        ),
        _ => draw_dot(
            canvas,
            badge_x + inset,
            badge_y + inset,
            badge_size - inset * 2,
            palette.badge_text,
        ),
    }
}

fn draw_digit_one(
    canvas: &mut Canvas,
    badge_x: i32,
    badge_y: i32,
    badge_size: i32,
    color: Color,
) {
    let stroke = (badge_size / 5).max(1);
    let center = badge_x + badge_size / 2;
    let top = badge_y + (badge_size / 5);
    let bottom = badge_y + badge_size - (badge_size / 5);

    canvas.fill_round_rect(
        Rect {
            x: center - stroke / 2,
            y: top,
            width: stroke,
            height: bottom - top,
        },
        stroke / 2,
        color,
    );
    canvas.fill_round_rect(
        Rect {
            x: center - stroke,
            y: top + stroke / 2,
            width: stroke + 1,
            height: stroke,
        },
        stroke / 2,
        color,
    );
}

fn draw_digit_two(
    canvas: &mut Canvas,
    badge_x: i32,
    badge_y: i32,
    badge_size: i32,
    color: Color,
) {
    let stroke = (badge_size / 5).max(1);
    let left = badge_x + (badge_size / 5);
    let top = badge_y + (badge_size / 5);
    let width = badge_size - (badge_size * 2 / 5);
    let middle = badge_y + badge_size / 2;
    let bottom = badge_y + badge_size - (badge_size / 5);

    canvas.fill_round_rect(
        Rect {
            x: left,
            y: top,
            width,
            height: stroke,
        },
        stroke / 2,
        color,
    );
    canvas.fill_round_rect(
        Rect {
            x: left + width - stroke,
            y: top,
            width: stroke,
            height: middle - top,
        },
        stroke / 2,
        color,
    );
    canvas.fill_round_rect(
        Rect {
            x: left,
            y: middle - stroke / 2,
            width,
            height: stroke,
        },
        stroke / 2,
        color,
    );
    canvas.fill_round_rect(
        Rect {
            x: left,
            y: middle,
            width: stroke,
            height: bottom - middle,
        },
        stroke / 2,
        color,
    );
    canvas.fill_round_rect(
        Rect {
            x: left,
            y: bottom - stroke,
            width,
            height: stroke,
        },
        stroke / 2,
        color,
    );
}

fn draw_dot(
    canvas: &mut Canvas,
    badge_x: i32,
    badge_y: i32,
    badge_size: i32,
    color: Color,
) {
    let dot_size = (badge_size / 3).max(1);
    let dot_x = badge_x + (badge_size - dot_size) / 2;
    let dot_y = badge_y + (badge_size - dot_size) / 2;
    canvas.fill_round_rect(
        Rect {
            x: dot_x,
            y: dot_y,
            width: dot_size,
            height: dot_size,
        },
        dot_size / 2,
        color,
    );
}
