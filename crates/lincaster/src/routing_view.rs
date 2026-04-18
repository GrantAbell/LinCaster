use egui::{Color32, FontId, Pos2, Rect, Rounding, Sense, Stroke, Vec2};
use lincaster_proto::StreamSnapshot;

use crate::dbus_client::BusInfo;

const NODE_WIDTH: f32 = 160.0;
const NODE_HEIGHT: f32 = 36.0;
const PORT_RADIUS: f32 = 6.0;
const STREAM_X: f32 = 60.0;
const NODE_SPACING: f32 = 54.0;
const TOP_MARGIN: f32 = 20.0;

const COLOR_STREAM_BG: Color32 = Color32::from_rgb(55, 90, 130);
const COLOR_STREAM_BORDER: Color32 = Color32::from_rgb(90, 140, 200);
const COLOR_SINK_BG: Color32 = Color32::from_rgb(45, 100, 75);
const COLOR_SINK_BORDER: Color32 = Color32::from_rgb(75, 160, 120);
const COLOR_DEFAULT_BG: Color32 = Color32::from_rgb(80, 80, 85);
const COLOR_DEFAULT_BORDER: Color32 = Color32::from_rgb(130, 130, 140);
const COLOR_CONNECTION: Color32 = Color32::from_rgb(180, 180, 195);
const COLOR_DRAG_LINE: Color32 = Color32::from_rgb(255, 200, 60);
const COLOR_PORT: Color32 = Color32::from_rgb(200, 200, 210);
const COLOR_PORT_HOVER: Color32 = Color32::from_rgb(255, 220, 80);
const COLOR_DROP_TARGET: Color32 = Color32::from_rgb(120, 255, 120);
const COLOR_TEXT: Color32 = Color32::from_rgb(230, 230, 235);
const COLOR_TEXT_DIM: Color32 = Color32::from_rgb(160, 160, 170);
const COLOR_AUTO_ROUTE: Color32 = Color32::from_rgb(220, 180, 60);

/// Sink node for display (our busses + the "Default" pseudo-sink).
#[derive(Clone)]
pub struct SinkNode {
    pub id: String,
    pub display_name: String,
    pub is_default: bool,
}

/// Ongoing drag state when the user is re-routing a stream.
pub struct DragState {
    pub stream_node_id: u32,
    pub from_pos: Pos2,
    pub current_pos: Pos2,
}

/// Actions the routing view can produce.
pub enum RoutingAction {
    Route(u32, Option<String>),
    SetManualOverride(bool),
}

/// Draws the routing view and returns an optional action.
pub fn draw_routing_view(
    ui: &mut egui::Ui,
    streams: &[StreamSnapshot],
    busses: &[BusInfo],
    drag: &mut Option<DragState>,
    manual_override: &mut bool,
) -> Option<RoutingAction> {
    let mut action: Option<RoutingAction> = None;

    // Manual override toggle at the top
    let has_auto_routed = streams.iter().any(|s| s.auto_routed);
    ui.horizontal(|ui| {
        let prev = *manual_override;
        ui.checkbox(manual_override, "Manual Override");
        if *manual_override != prev {
            action = Some(RoutingAction::SetManualOverride(*manual_override));
        }
        if has_auto_routed && !*manual_override {
            ui.label(
                egui::RichText::new("⚡ Some streams are auto-routed by config rules")
                    .color(COLOR_AUTO_ROUTE)
                    .size(12.0),
            );
        }
        if *manual_override {
            ui.label(
                egui::RichText::new("Config auto-routing disabled — manual routing only")
                    .color(Color32::from_rgb(120, 200, 120))
                    .size(12.0),
            );
        }
    });

    ui.add_space(4.0);

    let available = ui.available_rect_before_wrap();
    let response = ui.allocate_rect(available, Sense::click_and_drag());
    let painter = ui.painter_at(available);
    let mouse_pos = response.hover_pos();

    let sink_x = (available.max.x - NODE_WIDTH - 40.0).max(STREAM_X + NODE_WIDTH + 120.0);

    // Build sink list: our busses + "Default Audio"
    let mut sinks: Vec<SinkNode> = vec![SinkNode {
        id: "__default__".into(),
        display_name: "Default Audio".into(),
        is_default: true,
    }];
    for bus in busses {
        sinks.push(SinkNode {
            id: bus.bus_id.clone(),
            display_name: bus.display_name.clone(),
            is_default: false,
        });
    }

    // Calculate node positions
    let stream_positions: Vec<(u32, Rect)> = streams
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let y = available.min.y + TOP_MARGIN + i as f32 * NODE_SPACING;
            let rect = Rect::from_min_size(
                Pos2::new(available.min.x + STREAM_X, y),
                Vec2::new(NODE_WIDTH, NODE_HEIGHT),
            );
            (s.node_id, rect)
        })
        .collect();

    let sink_positions: Vec<(String, Rect)> = sinks
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let y = available.min.y + TOP_MARGIN + i as f32 * NODE_SPACING;
            let rect =
                Rect::from_min_size(Pos2::new(sink_x, y), Vec2::new(NODE_WIDTH, NODE_HEIGHT));
            (s.id.clone(), rect)
        })
        .collect();

    let dragging_stream_id = drag.as_ref().map(|d| d.stream_node_id);

    // Draw connections (bezier curves from stream ports to sink ports)
    for stream in streams {
        if dragging_stream_id == Some(stream.node_id) {
            continue; // Don't draw the existing connection while dragging
        }

        let stream_rect = stream_positions
            .iter()
            .find(|(id, _)| *id == stream.node_id)
            .map(|(_, r)| *r);
        let target_id = stream.target_bus_id.as_deref().unwrap_or("__default__");
        let sink_rect = sink_positions
            .iter()
            .find(|(id, _)| id == target_id)
            .map(|(_, r)| *r);

        if let (Some(sr), Some(tr)) = (stream_rect, sink_rect) {
            let from = Pos2::new(sr.max.x, sr.center().y);
            let to = Pos2::new(tr.min.x, tr.center().y);
            draw_connection(&painter, from, to, COLOR_CONNECTION, 2.0);
        }
    }

    // Draw drag line
    if let Some(ref d) = drag {
        draw_connection(&painter, d.from_pos, d.current_pos, COLOR_DRAG_LINE, 2.5);
    }

    // Draw stream nodes
    for stream in streams {
        let rect = match stream_positions
            .iter()
            .find(|(id, _)| *id == stream.node_id)
        {
            Some((_, r)) => *r,
            None => continue,
        };

        draw_node(
            &painter,
            rect,
            &stream.display_name,
            COLOR_STREAM_BG,
            COLOR_STREAM_BORDER,
        );

        // Pinned vs default indicator (small icon top-left of node)
        let is_pinned = stream.target_bus_id.is_some();
        let indicator_text = if stream.auto_routed {
            "⚡"
        } else if is_pinned {
            "📌"
        } else {
            "🔄"
        };
        let indicator_color = if stream.auto_routed {
            COLOR_AUTO_ROUTE
        } else {
            COLOR_TEXT_DIM
        };
        painter.text(
            Pos2::new(rect.min.x + 4.0, rect.min.y + 2.0),
            egui::Align2::LEFT_TOP,
            indicator_text,
            FontId::proportional(9.0),
            indicator_color,
        );

        // Output port circle (right edge)
        let port_center = Pos2::new(rect.max.x, rect.center().y);
        let port_hovered = mouse_pos
            .map(|mp| mp.distance(port_center) < PORT_RADIUS * 2.5)
            .unwrap_or(false);
        let port_color = if port_hovered {
            COLOR_PORT_HOVER
        } else {
            COLOR_PORT
        };
        painter.circle_filled(port_center, PORT_RADIUS, port_color);

        // Start drag on port click
        if port_hovered && response.drag_started() {
            *drag = Some(DragState {
                stream_node_id: stream.node_id,
                from_pos: port_center,
                current_pos: port_center,
            });
        }
    }

    // Draw sink nodes
    for (i, sink) in sinks.iter().enumerate() {
        let rect = sink_positions[i].1;

        let (bg, border) = if sink.is_default {
            (COLOR_DEFAULT_BG, COLOR_DEFAULT_BORDER)
        } else {
            (COLOR_SINK_BG, COLOR_SINK_BORDER)
        };

        // Highlight drop target while dragging
        let port_center = Pos2::new(rect.min.x, rect.center().y);
        let is_drop_target = drag.is_some()
            && mouse_pos
                .map(|mp| mp.distance(port_center) < PORT_RADIUS * 3.5 || rect.contains(mp))
                .unwrap_or(false);

        let actual_border = if is_drop_target {
            COLOR_DROP_TARGET
        } else {
            border
        };

        draw_node(&painter, rect, &sink.display_name, bg, actual_border);

        // Input port circle (left edge)
        let port_color = if is_drop_target {
            COLOR_DROP_TARGET
        } else {
            COLOR_PORT
        };
        painter.circle_filled(port_center, PORT_RADIUS, port_color);
    }

    // Update drag position
    if let Some(ref mut d) = drag {
        if let Some(mp) = mouse_pos {
            d.current_pos = mp;
        }
    }

    // Handle drag release
    if response.drag_stopped() {
        if let Some(d) = drag.take() {
            // Find which sink we dropped on
            if let Some(mp) = mouse_pos {
                for (i, sink) in sinks.iter().enumerate() {
                    let rect = sink_positions[i].1;
                    let port_center = Pos2::new(rect.min.x, rect.center().y);
                    if mp.distance(port_center) < PORT_RADIUS * 3.5 || rect.contains(mp) {
                        let target = if sink.is_default {
                            None
                        } else {
                            Some(sink.id.clone())
                        };
                        action = Some(RoutingAction::Route(d.stream_node_id, target));
                        break;
                    }
                }
            }
        }
    }

    // Show message when no streams are active
    if streams.is_empty() {
        painter.text(
            Pos2::new(
                available.min.x + STREAM_X + NODE_WIDTH / 2.0,
                available.min.y + TOP_MARGIN + NODE_HEIGHT / 2.0,
            ),
            egui::Align2::CENTER_CENTER,
            "No active audio streams",
            FontId::proportional(14.0),
            COLOR_TEXT_DIM,
        );
    }

    // Request repaint while dragging for smooth updates
    if drag.is_some() {
        ui.ctx().request_repaint();
    }

    action
}

fn draw_node(painter: &egui::Painter, rect: Rect, label: &str, bg: Color32, border: Color32) {
    painter.rect_filled(rect, Rounding::same(6.0), bg);
    painter.rect_stroke(rect, Rounding::same(6.0), Stroke::new(1.5, border));
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        label,
        FontId::proportional(13.5),
        COLOR_TEXT,
    );
}

fn draw_connection(painter: &egui::Painter, from: Pos2, to: Pos2, color: Color32, width: f32) {
    let dx = (to.x - from.x).abs() * 0.45;
    let ctrl1 = Pos2::new(from.x + dx, from.y);
    let ctrl2 = Pos2::new(to.x - dx, to.y);

    let bezier = egui::epaint::CubicBezierShape::from_points_stroke(
        [from, ctrl1, ctrl2, to],
        false,
        Color32::TRANSPARENT,
        Stroke::new(width, color),
    );
    painter.add(bezier);
}
