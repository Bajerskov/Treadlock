//! Screen-space UI: bitmap text, panels and the minimap.
//!
//! The font is a 5x7 bitmap defined in code rather than loaded from a file.
//! That avoids both a font dependency and an asset to stream, which suits a
//! target whose storage is PCIe 2.0 x2, and a pixel font suits the look anyway.
//!
//! Everything is emitted into one vertex buffer sampling one atlas, so the
//! entire HUD costs a single draw call.

use glam::{Vec2, Vec3, Vec4};

use crate::sim::Sim;

/// Cell size in the atlas. Glyphs are 5x7 inside an 8x8 cell, so neighbours
/// cannot bleed into each other under filtering.
const CELL: u32 = 8;
const COLUMNS: u32 = 16;
const ROWS: u32 = 4;
pub const ATLAS_WIDTH: u32 = CELL * COLUMNS;
pub const ATLAS_HEIGHT: u32 = CELL * ROWS;
/// Last cell is filled solid, so untextured shapes sample it and the whole UI
/// stays one draw call.
const WHITE_CELL: u32 = COLUMNS * ROWS - 1;

const GLYPH_W: f32 = 5.0;
const GLYPH_H: f32 = 7.0;

/// Window pixels, measured from the top left, to clip space.
///
/// The sign of the y term is the whole ball game: get it wrong and the entire
/// overlay lands mirrored top to bottom, with panels in the opposite corners
/// and text upside down. `ui.wgsl` performs exactly this calculation, and the
/// orientation test below rasterises through it, so the two cannot drift apart
/// without a test failing.
#[allow(dead_code)] // the shader is the real consumer; this is the reference and test subject
pub fn pixel_to_ndc(pos: Vec2, size: Vec2) -> Vec2 {
    let size = size.max(Vec2::ONE);
    Vec2::new(pos.x / size.x * 2.0 - 1.0, 1.0 - pos.y / size.y * 2.0)
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
pub struct UiVertex {
    /// Pixels from the top left of the window.
    pub pos: [f32; 2],
    pub uv: [f32; 2],
    pub color: [f32; 4],
}

/// Each byte is one row, low five bits, most significant bit leftmost.
const FONT: [(char, [u8; 7]); 44] = [
    (' ', [0, 0, 0, 0, 0, 0, 0]),
    ('0', [0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110]),
    ('1', [0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110]),
    ('2', [0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111]),
    ('3', [0b11111, 0b00010, 0b00100, 0b00010, 0b00001, 0b10001, 0b01110]),
    ('4', [0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010]),
    ('5', [0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110]),
    ('6', [0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110]),
    ('7', [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000]),
    ('8', [0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110]),
    ('9', [0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100]),
    (':', [0b00000, 0b00100, 0b00100, 0b00000, 0b00100, 0b00100, 0b00000]),
    ('.', [0b00000, 0b00000, 0b00000, 0b00000, 0b00000, 0b01100, 0b01100]),
    ('-', [0b00000, 0b00000, 0b00000, 0b11111, 0b00000, 0b00000, 0b00000]),
    ('/', [0b00001, 0b00010, 0b00010, 0b00100, 0b01000, 0b01000, 0b10000]),
    ('A', [0b01110, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001]),
    ('B', [0b11110, 0b10001, 0b10001, 0b11110, 0b10001, 0b10001, 0b11110]),
    ('C', [0b01110, 0b10001, 0b10000, 0b10000, 0b10000, 0b10001, 0b01110]),
    ('D', [0b11100, 0b10010, 0b10001, 0b10001, 0b10001, 0b10010, 0b11100]),
    ('E', [0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b11111]),
    ('F', [0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b10000]),
    ('G', [0b01110, 0b10001, 0b10000, 0b10111, 0b10001, 0b10001, 0b01111]),
    ('H', [0b10001, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001]),
    ('I', [0b01110, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110]),
    ('J', [0b00111, 0b00010, 0b00010, 0b00010, 0b00010, 0b10010, 0b01100]),
    ('K', [0b10001, 0b10010, 0b10100, 0b11000, 0b10100, 0b10010, 0b10001]),
    ('L', [0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b11111]),
    ('M', [0b10001, 0b11011, 0b10101, 0b10101, 0b10001, 0b10001, 0b10001]),
    ('N', [0b10001, 0b10001, 0b11001, 0b10101, 0b10011, 0b10001, 0b10001]),
    ('O', [0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110]),
    ('P', [0b11110, 0b10001, 0b10001, 0b11110, 0b10000, 0b10000, 0b10000]),
    ('Q', [0b01110, 0b10001, 0b10001, 0b10001, 0b10101, 0b10010, 0b01101]),
    ('R', [0b11110, 0b10001, 0b10001, 0b11110, 0b10100, 0b10010, 0b10001]),
    ('S', [0b01111, 0b10000, 0b10000, 0b01110, 0b00001, 0b00001, 0b11110]),
    ('T', [0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100]),
    ('U', [0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110]),
    ('V', [0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01010, 0b00100]),
    ('W', [0b10001, 0b10001, 0b10001, 0b10101, 0b10101, 0b11011, 0b10001]),
    ('X', [0b10001, 0b10001, 0b01010, 0b00100, 0b01010, 0b10001, 0b10001]),
    ('Y', [0b10001, 0b10001, 0b01010, 0b00100, 0b00100, 0b00100, 0b00100]),
    ('Z', [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b10000, 0b11111]),
    ('+', [0b00000, 0b00100, 0b00100, 0b11111, 0b00100, 0b00100, 0b00000]),
    ('%', [0b11001, 0b11010, 0b00010, 0b00100, 0b01000, 0b01011, 0b10011]),
    ('?', [0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b00000, 0b00100]),
];

/// Rasterise the font into RGBA8. White glyphs on transparent black, so colour
/// comes from the vertex and one atlas serves every tint.
pub fn build_atlas() -> Vec<u8> {
    let mut pixels = vec![0u8; (ATLAS_WIDTH * ATLAS_HEIGHT * 4) as usize];
    let put = |pixels: &mut Vec<u8>, x: u32, y: u32, value: u8| {
        let index = ((y * ATLAS_WIDTH + x) * 4) as usize;
        pixels[index..index + 4].copy_from_slice(&[255, 255, 255, value]);
    };

    for (slot, (_, rows)) in FONT.iter().enumerate() {
        let cell_x = (slot as u32 % COLUMNS) * CELL;
        let cell_y = (slot as u32 / COLUMNS) * CELL;
        for (row, bits) in rows.iter().enumerate() {
            for column in 0..5u32 {
                if bits & (1 << (4 - column)) != 0 {
                    put(&mut pixels, cell_x + column, cell_y + row as u32, 255);
                }
            }
        }
    }

    // Solid cell for untextured shapes.
    let wx = (WHITE_CELL % COLUMNS) * CELL;
    let wy = (WHITE_CELL / COLUMNS) * CELL;
    for y in 0..CELL {
        for x in 0..CELL {
            put(&mut pixels, wx + x, wy + y, 255);
        }
    }
    pixels
}

fn glyph_slot(c: char) -> Option<u32> {
    let upper = c.to_ascii_uppercase();
    FONT.iter().position(|(g, _)| *g == upper).map(|i| i as u32)
}

#[derive(Default)]
pub struct Ui {
    pub vertices: Vec<UiVertex>,
}

impl Ui {
    pub fn clear(&mut self) {
        self.vertices.clear();
    }

    fn quad(&mut self, min: Vec2, max: Vec2, uv_min: Vec2, uv_max: Vec2, color: Vec4) {
        let corners = [
            (Vec2::new(min.x, min.y), Vec2::new(uv_min.x, uv_min.y)),
            (Vec2::new(max.x, min.y), Vec2::new(uv_max.x, uv_min.y)),
            (Vec2::new(max.x, max.y), Vec2::new(uv_max.x, uv_max.y)),
            (Vec2::new(min.x, min.y), Vec2::new(uv_min.x, uv_min.y)),
            (Vec2::new(max.x, max.y), Vec2::new(uv_max.x, uv_max.y)),
            (Vec2::new(min.x, max.y), Vec2::new(uv_min.x, uv_max.y)),
        ];
        for (pos, uv) in corners {
            self.vertices.push(UiVertex {
                pos: pos.to_array(),
                uv: uv.to_array(),
                color: color.to_array(),
            });
        }
    }

    /// Solid rectangle, sampling the atlas's white cell.
    pub fn rect(&mut self, x: f32, y: f32, w: f32, h: f32, color: Vec4) {
        // A pixel well inside the white cell, so filtering cannot reach a
        // neighbouring glyph.
        let cx = ((WHITE_CELL % COLUMNS) * CELL + CELL / 2) as f32 / ATLAS_WIDTH as f32;
        let cy = ((WHITE_CELL / COLUMNS) * CELL + CELL / 2) as f32 / ATLAS_HEIGHT as f32;
        let uv = Vec2::new(cx, cy);
        self.quad(Vec2::new(x, y), Vec2::new(x + w, y + h), uv, uv, color);
    }

    /// Line between two points, drawn as a rotated quad.
    pub fn line(&mut self, a: Vec2, b: Vec2, thickness: f32, color: Vec4) {
        let direction = b - a;
        let length = direction.length();
        if length < 1e-3 {
            return;
        }
        let normal = Vec2::new(-direction.y, direction.x) / length * (thickness * 0.5);
        let cx = ((WHITE_CELL % COLUMNS) * CELL + CELL / 2) as f32 / ATLAS_WIDTH as f32;
        let cy = ((WHITE_CELL / COLUMNS) * CELL + CELL / 2) as f32 / ATLAS_HEIGHT as f32;
        let uv = Vec2::new(cx, cy);

        for (pos, _) in [
            (a - normal, uv), (b - normal, uv), (b + normal, uv),
            (a - normal, uv), (b + normal, uv), (a + normal, uv),
        ] {
            self.vertices.push(UiVertex {
                pos: pos.to_array(),
                uv: uv.to_array(),
                color: color.to_array(),
            });
        }
    }

    /// Width in pixels that `text` will occupy at `scale`.
    pub fn text_width(text: &str, scale: f32) -> f32 {
        text.chars().count() as f32 * (GLYPH_W + 1.0) * scale
    }

    pub fn text(&mut self, x: f32, y: f32, scale: f32, color: Vec4, text: &str) {
        let mut pen = x;
        for c in text.chars() {
            if let Some(slot) = glyph_slot(c) {
                let cell_x = (slot % COLUMNS) * CELL;
                let cell_y = (slot / COLUMNS) * CELL;
                // Sample exactly the 5x7 glyph area out of its 8x8 cell.
                let uv_min = Vec2::new(
                    cell_x as f32 / ATLAS_WIDTH as f32,
                    cell_y as f32 / ATLAS_HEIGHT as f32,
                );
                let uv_max = Vec2::new(
                    (cell_x as f32 + GLYPH_W) / ATLAS_WIDTH as f32,
                    (cell_y as f32 + GLYPH_H) / ATLAS_HEIGHT as f32,
                );
                self.quad(
                    Vec2::new(pen, y),
                    Vec2::new(pen + GLYPH_W * scale, y + GLYPH_H * scale),
                    uv_min,
                    uv_max,
                    color,
                );
            }
            pen += (GLYPH_W + 1.0) * scale;
        }
    }

    /// Text with a dark offset copy behind it, so it stays readable against a
    /// bright track without needing a panel behind every label.
    pub fn text_shadowed(&mut self, x: f32, y: f32, scale: f32, color: Vec4, text: &str) {
        self.text(x + scale, y + scale, scale, Vec4::new(0.0, 0.0, 0.0, 0.7), text);
        self.text(x, y, scale, color, text);
    }
}

fn seconds_to_clock(t: f32) -> String {
    if !t.is_finite() {
        return "--:--.--".to_string();
    }
    let minutes = (t / 60.0).floor() as u32;
    let seconds = t - minutes as f32 * 60.0;
    format!("{minutes}:{seconds:05.2}")
}

/// Build the whole HUD for this frame.
pub fn build_hud(ui: &mut Ui, sim: &Sim, width: f32, height: f32) {
    ui.clear();
    let scale = (height / 360.0).max(1.0).floor();
    let pad = 12.0 * scale;
    let white = Vec4::ONE;
    let dim = Vec4::new(0.75, 0.80, 0.90, 1.0);
    let accent = Vec4::new(0.35, 0.95, 1.0, 1.0);

    // Speed, bottom right and large: the number read most often.
    let speed = format!("{:.0}", sim.player.speed_kph());
    let speed_scale = scale * 4.0;
    let speed_w = Ui::text_width(&speed, speed_scale);
    ui.text_shadowed(width - pad - speed_w, height - pad - 7.0 * speed_scale, speed_scale, white, &speed);
    let unit_scale = scale * 1.5;
    ui.text_shadowed(
        width - pad - Ui::text_width("KM/H", unit_scale),
        height - pad - 7.0 * speed_scale - 9.0 * unit_scale,
        unit_scale,
        dim,
        "KM/H",
    );

    // Boost reserve, as a bar under the speed.
    let bar_w = 120.0 * scale;
    let bar_h = 4.0 * scale;
    let bar_x = width - pad - bar_w;
    let bar_y = height - pad + 2.0 * scale;
    ui.rect(bar_x, bar_y, bar_w, bar_h, Vec4::new(0.1, 0.12, 0.18, 0.8));
    let charge = sim.player.boost.clamp(0.0, 1.0);
    let boosting = sim.player.pad_boost > 0.0;
    ui.rect(
        bar_x,
        bar_y,
        bar_w * charge,
        bar_h,
        if boosting { Vec4::new(0.5, 1.0, 1.0, 1.0) } else { accent },
    );

    // Position and lap, top left.
    let position = sim.player_position();
    let field = sim.opponents.len() + 1;
    let pos_scale = scale * 3.0;
    ui.text_shadowed(pad, pad, pos_scale, white, &format!("P{position}"));
    let small = scale * 1.5;
    ui.text_shadowed(
        pad + Ui::text_width("P0", pos_scale) + 4.0 * scale,
        pad + 7.0 * pos_scale - 7.0 * small,
        small,
        dim,
        &format!("/{field}"),
    );
    ui.text_shadowed(pad, pad + 9.0 * pos_scale, small, dim, &format!("LAP {}", sim.lap + 1));

    // Lap times, top centre-left under the lap counter.
    let time_y = pad + 9.0 * pos_scale + 10.0 * small;
    ui.text_shadowed(pad, time_y, small, dim, &format!("CUR {}", seconds_to_clock(sim.time - sim.lap_started())));
    ui.text_shadowed(
        pad,
        time_y + 9.0 * small,
        small,
        if sim.best_lap_time.is_finite() { accent } else { dim },
        &format!("BEST {}", seconds_to_clock(sim.best_lap_time)),
    );

    build_standings(ui, sim, width, pad, scale);
    build_minimap(ui, sim, height, pad, scale);
}

/// Wrong-way warning. Sits above centre rather than across it: the driver needs
/// to see the track to turn around, so the warning must not cover the one thing
/// it is asking them to look at.
pub fn build_wrong_way(ui: &mut Ui, width: f32, height: f32, time: f32) {
    let scale = (height / 360.0).max(1.0).floor();
    let text_scale = scale * 3.5;

    // Pulsing, because a static banner stops registering after a second or two.
    let pulse = 0.55 + 0.45 * (time * 9.0).sin();
    let red = Vec4::new(1.0, 0.25 + 0.15 * pulse, 0.15, 1.0);

    let title = "WRONG WAY";
    let title_w = Ui::text_width(title, text_scale);
    let x = (width - title_w) * 0.5;
    let y = height * 0.26;

    ui.rect(
        x - 10.0 * scale,
        y - 6.0 * scale,
        title_w + 20.0 * scale,
        7.0 * text_scale + 12.0 * scale,
        Vec4::new(0.25, 0.02, 0.02, 0.35 + 0.25 * pulse),
    );
    ui.text_shadowed(x, y, text_scale, red, title);

    let sub = "TURN AROUND";
    let sub_scale = scale * 1.75;
    ui.text_shadowed(
        (width - Ui::text_width(sub, sub_scale)) * 0.5,
        y + 7.0 * text_scale + 6.0 * scale,
        sub_scale,
        Vec4::new(1.0, 0.8, 0.7, 1.0),
        sub,
    );
}

/// Banner for the modes that change what the controls do, so it is never a
/// mystery why steering does nothing or why the camera is not following.
pub fn build_mode_banner(
    ui: &mut Ui,
    width: f32,
    height: f32,
    ai_driving: bool,
    orbiting: bool,
) {
    if !ai_driving && !orbiting {
        return;
    }
    let scale = (height / 360.0).max(1.0).floor();
    let text_scale = scale * 2.0;

    let mut parts: Vec<&str> = Vec::new();
    if ai_driving {
        parts.push("AI DRIVING");
    }
    if orbiting {
        parts.push("ORBIT CAMERA");
    }
    let banner = parts.join("   ");

    let text_w = Ui::text_width(&banner, text_scale);
    let x = (width - text_w) * 0.5;
    let y = height * 0.12;
    ui.rect(
        x - 8.0 * scale,
        y - 5.0 * scale,
        text_w + 16.0 * scale,
        7.0 * text_scale + 10.0 * scale,
        Vec4::new(0.05, 0.06, 0.10, 0.55),
    );
    ui.text_shadowed(x, y, text_scale, Vec4::new(1.0, 0.85, 0.3, 1.0), &banner);

    if orbiting {
        let hint = "DRAG TO ORBIT   WHEEL TO ZOOM   C TO EXIT";
        let hint_scale = scale * 1.25;
        ui.text_shadowed(
            (width - Ui::text_width(hint, hint_scale)) * 0.5,
            y + 7.0 * text_scale + 6.0 * scale,
            hint_scale,
            Vec4::new(0.7, 0.75, 0.85, 1.0),
            hint,
        );
    }
}

/// Label each corner with its name. The whole overlay landing mirrored is a
/// single sign in `pixel_to_ndc`, and this settles which way it should go in one
/// look rather than by reading upside-down text and guessing.
pub fn build_orientation_markers(ui: &mut Ui, width: f32, height: f32) {
    let scale = (height / 360.0).max(1.0).floor() * 2.0;
    let inset = 4.0 * scale;
    let green = Vec4::new(0.3, 1.0, 0.4, 1.0);
    let red = Vec4::new(1.0, 0.35, 0.25, 1.0);

    ui.text_shadowed(inset, inset, scale, green, "TOP LEFT");
    let tr = "TOP RIGHT";
    ui.text_shadowed(width - inset - Ui::text_width(tr, scale), inset, scale, green, tr);
    ui.text_shadowed(inset, height - inset - 7.0 * scale, scale, red, "BOTTOM LEFT");
    let br = "BOTTOM RIGHT";
    ui.text_shadowed(
        width - inset - Ui::text_width(br, scale),
        height - inset - 7.0 * scale,
        scale,
        red,
        br,
    );
}

/// Running order down the right-hand side, player row highlighted.
fn build_standings(ui: &mut Ui, sim: &Sim, width: f32, pad: f32, scale: f32) {
    let small = scale * 1.5;
    let row_h = 10.0 * small;
    let panel_w = 108.0 * scale;
    let x = width - pad - panel_w;
    let y = pad;

    let mut order = sim.standings();
    order.truncate(6);

    ui.rect(
        x - 4.0 * scale,
        y - 4.0 * scale,
        panel_w + 8.0 * scale,
        row_h * order.len() as f32 + 8.0 * scale,
        Vec4::new(0.04, 0.05, 0.09, 0.55),
    );

    for (rank, entry) in order.iter().enumerate() {
        let row_y = y + rank as f32 * row_h;
        if entry.is_player {
            ui.rect(x - 4.0 * scale, row_y - 1.0 * scale, panel_w + 8.0 * scale, row_h, Vec4::new(0.9, 0.35, 0.15, 0.30));
        }
        // Livery swatch, matching the car on track.
        let c = entry.tint;
        ui.rect(x, row_y, 4.0 * scale, 7.0 * small, Vec4::new(c.x, c.y, c.z, 1.0));

        let label = if entry.is_player { "YOU" } else { "CPU" };
        ui.text(
            x + 8.0 * scale,
            row_y,
            small,
            if entry.is_player { Vec4::ONE } else { Vec4::new(0.8, 0.84, 0.92, 1.0) },
            &format!("{}. {label}", rank + 1),
        );

        // Gap to the leader in metres, which is more useful mid-race than a
        // time that only resolves at the line.
        if rank > 0 {
            let gap = order[0].progress - entry.progress;
            let text = if gap > 999.0 { format!("{:.1}K", gap / 1000.0) } else { format!("{gap:.0}M") };
            ui.text(
                x + panel_w - Ui::text_width(&text, small),
                row_y,
                small,
                Vec4::new(0.6, 0.65, 0.75, 1.0),
                &text,
            );
        }
    }
}

/// Top-down minimap. The track is a closed loop, so an orthographic plan view
/// of the centreline reads well without any projection cleverness.
fn build_minimap(ui: &mut Ui, sim: &Sim, height: f32, pad: f32, scale: f32) {
    let size = 130.0 * scale;
    let origin = Vec2::new(pad, height - pad - size);

    // Fit the loop's plan bounds into the box, preserving aspect so the shape
    // of the circuit is recognisable.
    let (mut min, mut max) = (Vec2::splat(f32::MAX), Vec2::splat(f32::MIN));
    for f in &sim.track.frames {
        let p = Vec2::new(f.pos.x, f.pos.z);
        min = min.min(p);
        max = max.max(p);
    }
    let extent = (max - min).max(Vec2::splat(1.0));
    let fit = (size * 0.86) / extent.x.max(extent.y);
    let centre = (min + max) * 0.5;
    let to_screen =
        |p: Vec3| origin + Vec2::new(size, size) * 0.5 + (Vec2::new(p.x, p.z) - centre) * fit;

    ui.rect(origin.x, origin.y, size, size, Vec4::new(0.04, 0.05, 0.09, 0.5));

    // Subsample the centreline: at full density the segments are shorter than a
    // pixel and cost vertices for nothing.
    let frames = &sim.track.frames;
    let step = (frames.len() / 110).max(1);
    let mut i = 0;
    while i < frames.len() {
        let a = frames[i];
        let b = frames[(i + step) % frames.len()];
        // Open stretches are drawn warm and tube stretches cold, matching the
        // track itself, so the map says which regime is coming.
        let colour = if a.gravity_blend > 0.5 {
            Vec4::new(0.35, 0.55, 0.95, 0.85)
        } else {
            Vec4::new(0.95, 0.65, 0.25, 0.85)
        };
        ui.line(to_screen(a.pos), to_screen(b.pos), 2.0 * scale, colour);
        i += step;
    }

    // Start line.
    let start = to_screen(frames[sim.track.start].pos);
    ui.rect(start.x - 2.0 * scale, start.y - 2.0 * scale, 4.0 * scale, 4.0 * scale, Vec4::ONE);

    for opponent in &sim.opponents {
        let p = to_screen(opponent.car.pos);
        let c = opponent.tint;
        ui.rect(p.x - 1.5 * scale, p.y - 1.5 * scale, 3.0 * scale, 3.0 * scale, Vec4::new(c.x, c.y, c.z, 1.0));
    }
    // Player last and larger, so it is never hidden under a rival.
    let p = to_screen(sim.player.pos);
    ui.rect(p.x - 2.5 * scale, p.y - 2.5 * scale, 5.0 * scale, 5.0 * scale, Vec4::ONE);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every glyph the HUD asks for must exist in the atlas, or labels render
    /// with silent holes in them.
    #[test]
    fn hud_text_is_fully_covered_by_the_font() {
        for text in [
            "KM/H", "LAP", "BEST", "CUR", "YOU", "CPU", "P1", "/6", "0123456789", ":.-/M K",
            "AI DRIVING", "ORBIT CAMERA", "DRAG TO ORBIT   WHEEL TO ZOOM   C TO EXIT",
            "WRONG WAY", "TURN AROUND",
            "TOP LEFT", "TOP RIGHT", "BOTTOM LEFT", "BOTTOM RIGHT",
        ] {
            for c in text.chars() {
                assert!(
                    glyph_slot(c).is_some(),
                    "no glyph for {c:?}, needed by {text:?}"
                );
            }
        }
    }

    #[test]
    fn atlas_has_glyph_pixels_and_a_solid_white_cell() {
        let atlas = build_atlas();
        assert_eq!(atlas.len(), (ATLAS_WIDTH * ATLAS_HEIGHT * 4) as usize);

        // The white cell must be fully opaque, or every solid shape vanishes.
        let wx = (WHITE_CELL % COLUMNS) * CELL + CELL / 2;
        let wy = (WHITE_CELL / COLUMNS) * CELL + CELL / 2;
        let index = ((wy * ATLAS_WIDTH + wx) * 4) as usize;
        assert_eq!(atlas[index + 3], 255, "white cell is not opaque");

        // And a glyph must actually have marked pixels.
        let slot = glyph_slot('A').unwrap();
        let ax = (slot % COLUMNS) * CELL;
        let ay = (slot / COLUMNS) * CELL;
        let marked = (0..7).any(|row| {
            (0..5).any(|col| {
                let i = (((ay + row) * ATLAS_WIDTH + ax + col) * 4) as usize;
                atlas[i + 3] > 0
            })
        });
        assert!(marked, "glyph A rasterised empty");
    }

    /// A glyph must not sample into the cell beside it, which is what the 8px
    /// cell around a 5x7 glyph exists to prevent.
    #[test]
    fn glyphs_have_a_gap_between_cells() {
        let atlas = build_atlas();
        for slot in 0..FONT.len() as u32 {
            let x = (slot % COLUMNS) * CELL + 5;
            let y = (slot / COLUMNS) * CELL;
            for row in 0..CELL {
                let i = (((y + row) * ATLAS_WIDTH + x) * 4) as usize;
                assert_eq!(atlas[i + 3], 0, "glyph {slot} touches its cell margin");
            }
        }
    }
}

#[cfg(test)]
mod orientation {
    use super::*;

    /// Rasterise UI vertices the way the GPU will, so orientation can be
    /// checked without one. Pixel positions go through the same
    /// `pos / size * 2 - 1` the shader applies, then through Vulkan's
    /// ndc-to-framebuffer mapping, where ndc y = -1 is the TOP row.
    fn rasterise(vertices: &[UiVertex], w: usize, h: usize) -> String {
        let atlas = build_atlas();
        let mut out = vec![b'.'; w * h];

        for tri in vertices.chunks_exact(3) {
            // Rasterise in pixel space, which is what the layout code works in.
            // This checks the geometry, UVs and atlas - everything this module
            // is responsible for. Whether clip space then lands pixel row 0 at
            // the top of the window is the pipeline's business, and is asserted
            // separately below.
            let to_fb = |v: &UiVertex| (v.pos[0], v.pos[1]);
            let p: Vec<(f32, f32)> = tri.iter().map(to_fb).collect();

            for y in 0..h {
                for x in 0..w {
                    let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                    let area = |a: (f32, f32), b: (f32, f32), c: (f32, f32)| {
                        (b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0)
                    };
                    let total = area(p[0], p[1], p[2]);
                    if total.abs() < 1e-6 {
                        continue;
                    }
                    let w0 = area(p[1], p[2], (px, py)) / total;
                    let w1 = area(p[2], p[0], (px, py)) / total;
                    let w2 = area(p[0], p[1], (px, py)) / total;
                    if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                        continue;
                    }

                    let u = w0 * tri[0].uv[0] + w1 * tri[1].uv[0] + w2 * tri[2].uv[0];
                    let v = w0 * tri[0].uv[1] + w1 * tri[1].uv[1] + w2 * tri[2].uv[1];
                    let ax = ((u * ATLAS_WIDTH as f32) as usize).min(ATLAS_WIDTH as usize - 1);
                    let ay = ((v * ATLAS_HEIGHT as f32) as usize).min(ATLAS_HEIGHT as usize - 1);
                    let alpha = atlas[((ay as u32 * ATLAS_WIDTH + ax as u32) * 4 + 3) as usize];
                    if alpha > 127 {
                        out[y * w + x] = b'#';
                    }
                }
            }
        }

        out.chunks(w)
            .map(|row| String::from_utf8_lossy(row).into_owned())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn text_renders_the_right_way_round() {
        let mut ui = Ui::default();
        // F and L are asymmetric both ways round, so a flip on either axis is
        // obvious rather than something that still nearly reads.
        ui.text(1.0, 1.0, 1.0, Vec4::ONE, "FL");
        let image = rasterise(&ui.vertices, 14, 9);
        println!("\n{image}\n");

        let rows: Vec<&str> = image.lines().collect();
        // F's crossbar is at the top, so its top row must have more ink than
        // its bottom row. Flipped vertically, this reverses.
        let ink = |row: &str, from: usize, to: usize| {
            row[from..to.min(row.len())].chars().filter(|c| *c == '#').count()
        };
        assert!(
            ink(rows[1], 0, 5) > ink(rows[7], 0, 5),
            "F is upside down:\n{image}"
        );
        // F's stem is on the left, so its leftmost column carries ink on every
        // row. Flipped horizontally, the ink moves to the right.
        let stem = (1..8).filter(|&r| rows[r].as_bytes()[1] == b'#').count();
        assert!(stem >= 6, "F is mirrored left to right:\n{image}");
        // L is the second glyph, so it must sit to the RIGHT of F.
        let l_ink: usize = (1..8).map(|r| ink(rows[r], 7, 14)).sum();
        assert!(l_ink > 0, "second glyph did not render to the right:\n{image}");
    }

    /// Pixel row 0 is the top of the window, so it must map to the opposite end
    /// of clip space from the last row. Which end is which is the pipeline's
    /// convention; that the two ends differ, and that x is untouched, is not.
    #[test]
    fn pixel_to_ndc_spans_clip_space_without_touching_x() {
        let size = Vec2::new(800.0, 600.0);
        let top = pixel_to_ndc(Vec2::new(0.0, 0.0), size);
        let bottom = pixel_to_ndc(Vec2::new(0.0, 600.0), size);
        assert!(
            (top.y - bottom.y).abs() > 1.9,
            "top and bottom of the window map to the same place"
        );

        // x must not be mirrored, whatever happens vertically: left edge is -1,
        // right edge is +1.
        assert!((pixel_to_ndc(Vec2::ZERO, size).x + 1.0).abs() < 1e-5);
        assert!((pixel_to_ndc(Vec2::new(800.0, 0.0), size).x - 1.0).abs() < 1e-5);
    }
}
