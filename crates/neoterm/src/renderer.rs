use std::collections::HashMap;
use std::sync::Arc;

use bytemuck::{Pod, Zeroable};
use cosmic_text::{AttrsList, BufferLine, LineEnding, Style, Weight};
use glyphon::{
    Attrs, Buffer, Cache, Color, Family, FontSystem, Metrics, Resolution, Shaping, SwashCache,
    TextArea, TextAtlas, TextBounds, TextRenderer, Viewport,
};
use wgpu::{
    CommandEncoderDescriptor, CompositeAlphaMode, Device, DeviceDescriptor, Instance,
    InstanceDescriptor, LoadOp, MultisampleState, Operations, PresentMode,
    RenderPassColorAttachment, RenderPassDescriptor, RequestAdapterOptions, Surface,
    SurfaceConfiguration, TextureFormat, TextureUsages, TextureViewDescriptor,
};
use winit::{dpi::LogicalSize, window::Window};

use crate::config::NeotermConfig;
use crate::mouse;
use crate::tab::{CellRect, PaneId, RowContent};

/// One pane's drawable content for a frame: grid rect (body-relative
/// cells) plus resolved rows. Built per frame in main from the tab layout.
pub struct PaneDraw {
    pub id: PaneId,
    pub rect: CellRect,
    pub rows: Vec<RowContent>,
    /// Cursor cell as (viewport row, grid column). The renderer draws the
    /// block here, behind the glyph, so the character stays readable.
    pub cursor: Option<(usize, usize)>,
    /// This pane holds the keyboard focus (outlined; the others recede).
    pub focused: bool,
}

/// Viewport scroll state driving the scrollbar (built per frame in main).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScrollState {
    /// Total scrollback history lines.
    pub history: usize,
    /// Lines scrolled up (0 = live view).
    pub offset: usize,
    /// Viewport height in rows.
    pub rows: usize,
    /// Active pane rect (grid cells): the track lives inside it.
    /// None falls back to the full body (should not happen in practice).
    pub pane: Option<CellRect>,
}

/// Max background rects per frame: selection + scrollbar + per-cell bg.
/// Rows clamp at 200 and vivid apps rarely exceed a few segments per row;
/// overflow is truncated (documented, never crashes).
const MAX_BG_RECTS: usize = 2048;
const SEL_VERTS_PER_RECT: usize = 6;
const SEL_FLOATS_PER_VERT: usize = 6;

/// Body padding in logical pixels at scale 1. Symmetric so the grid sits
/// in an even frame instead of hugging the window edges.
pub const PAD_X: f32 = 12.0;
pub const PAD_TOP: f32 = 10.0;
pub const PAD_BOTTOM: f32 = 10.0;
/// Tab strip: left inset and minimum height (logical px at scale 1).
/// The strip grows with the font so large zoom levels never clip labels.
pub const TAB_LEFT: f32 = 12.0;
pub const STRIP_MIN_H: f32 = 32.0;
/// Terminal line height as a multiple of the font size.
pub const LINE_HEIGHT: f32 = 1.35;

/// One colored 2D vertex for the selection-background pass. Alpha lets
/// chrome (selection, dividers, scrollbar) blend softly into the body
/// instead of stamping hard color blocks.
#[repr(C)]
#[derive(Copy, Clone, Debug, Pod, Zeroable)]
struct SelVertex {
    pos: [f32; 2],
    col: [f32; 4],
}

const SEL_SHADER: &str = r#"
struct Vin {
    @location(0) pos: vec2<f32>,
    @location(1) col: vec4<f32>,
};
struct Vout {
    @builtin(position) pos: vec4<f32>,
    @location(0) col: vec4<f32>,
};
@vertex
fn vs(v: Vin) -> Vout {
    var o: Vout;
    o.pos = vec4<f32>(v.pos, 0.0, 1.0);
    o.col = v.col;
    return o;
}
@fragment
fn fs(@location(0) col: vec4<f32>) -> @location(0) vec4<f32> {
    return col;
}
"#;

pub struct Renderer {
    device: Device,
    queue: wgpu::Queue,
    surface: Surface<'static>,
    surface_config: SurfaceConfiguration,
    instance: Instance,
    font_system: FontSystem,
    swash_cache: SwashCache,
    viewport: Viewport,
    atlas: TextAtlas,
    text_renderer: TextRenderer,
    tab_buffer: Buffer,
    /// Text buffers + shaped-row caches per pane id (rebuilt on row-count
    /// change; pruned when panes close).
    pane_buffers: HashMap<PaneId, Vec<Buffer>>,
    pane_rows: HashMap<PaneId, Vec<RowContent>>,
    window: Arc<Window>,
    bg: wgpu::Color,
    /// Visual-bell clear color: the body background tinted toward the
    /// cursor color.
    flash_bg: wgpu::Color,
    /// Visual bell active for this frame.
    flash: bool,
    fg: Color,
    font_size: f32,
    /// Resolved font family; `None` = cosmic-text's generic `Monospace`.
    font_family: Option<String>,
    /// Cursor block color (0..1 RGB, linear).
    cursor_rgb: [f32; 3],
    /// Body background as a quad color (0..1 RGB, linear) — used for the
    /// scrim that recedes unfocused panes.
    bg_quad: [f32; 3],
    /// Theme quad colors (0..1 RGB): selection, scrollbar, tab strip,
    /// active-tab accent.
    sel_bg: [f32; 3],
    track_bg: [f32; 3],
    thumb_bg: [f32; 3],
    strip_bg: [f32; 3],
    tab_accent: [f32; 3],
    /// Tab-strip text colors: active / inactive / close mark.
    tab_active_fg: Color,
    tab_inactive_fg: Color,
    tab_close_fg: Color,
    // Perf gate: skip cosmic-text shaping when content is unchanged.
    last_tab_text: String,
    /// Measured monospace advance in layout units (None until a non-empty
    /// row has been shaped; falls back to the 0.62 heuristic).
    measured_advance: Option<f32>,
    /// Solid-rect pipeline for backgrounds (cell bg, selection, scrollbar).
    bg_pipeline: wgpu::RenderPipeline,
    bg_buffer: wgpu::Buffer,
}

impl Renderer {
    pub async fn new(
        window: Arc<Window>,
        event_loop: &winit::event_loop::ActiveEventLoop,
        cfg: &NeotermConfig,
    ) -> Self {
        let physical_size = window.inner_size();

        let instance = Instance::new(InstanceDescriptor::new_with_display_handle(Box::new(
            event_loop.owned_display_handle(),
        )));
        let adapter = instance
            .request_adapter(&RequestAdapterOptions::default())
            .await
            .expect("no GPU adapter");
        let (device, queue) = adapter
            .request_device(&DeviceDescriptor::default())
            .await
            .expect("request device");

        let surface = instance
            .create_surface(window.clone())
            .expect("create surface");
        let swapchain_format = TextureFormat::Bgra8UnormSrgb;
        let surface_config = SurfaceConfiguration {
            usage: TextureUsages::RENDER_ATTACHMENT,
            format: swapchain_format,
            width: physical_size.width.max(1),
            height: physical_size.height.max(1),
            present_mode: PresentMode::Fifo,
            alpha_mode: CompositeAlphaMode::Opaque,
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
            color_space: wgpu::SurfaceColorSpace::Auto,
        };
        surface.configure(&device, &surface_config);

        let mut font_system = FontSystem::new();
        let swash_cache = SwashCache::new();
        let cache = Cache::new(&device);
        let viewport = Viewport::new(&device, &cache);
        let mut atlas = TextAtlas::new(&device, &queue, &cache, swapchain_format);
        let text_renderer =
            TextRenderer::new(&mut atlas, &device, MultisampleState::default(), None);

        let font_family = resolve_family(&mut font_system, &cfg.font_family);
        let font_size = cfg.font_size;
        let mut tab_buffer = Buffer::new(&mut font_system, Metrics::new(font_size, font_size * 1.3));
        tab_buffer.set_size(Some(2000.0), Some(40.0));

        // Solid-rect pipeline for selection backgrounds.
        let sel_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("neoterm-bg"),
            source: wgpu::ShaderSource::Wgsl(SEL_SHADER.into()),
        });
        let sel_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("neoterm-bg-layout"),
            bind_group_layouts: &[],
            immediate_size: 0,
        });
        let bg_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("neoterm-bg"),
            layout: Some(&sel_layout),
            vertex: wgpu::VertexState {
                module: &sel_shader,
                entry_point: Some("vs"),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: (SEL_FLOATS_PER_VERT * 4) as u64,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x4],
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &sel_shader,
                entry_point: Some("fs"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: swapchain_format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            multiview_mask: None,
            cache: None,
        });
        let bg_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("neoterm-bg-verts"),
            size: (MAX_BG_RECTS * SEL_VERTS_PER_RECT * SEL_FLOATS_PER_VERT * 4) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let mut renderer = Self {
            device,
            queue,
            surface,
            surface_config,
            instance,
            font_system,
            swash_cache,
            viewport,
            atlas,
            text_renderer,
            tab_buffer,
            pane_buffers: HashMap::new(),
            pane_rows: HashMap::new(),
            window,
            bg: cfg.bg_wgpu(),
            flash_bg: flash_color(cfg),
            flash: false,
            fg: cfg.fg_glyphon(),
            font_size,
            font_family,
            cursor_rgb: cursor_color(cfg),
            bg_quad: bg_color(cfg),
            sel_bg: cfg.theme.selection_quad(),
            track_bg: cfg.theme.scrollbar_track_quad(),
            thumb_bg: cfg.theme.scrollbar_thumb_quad(),
            strip_bg: cfg.theme.tab_bar_bg_quad(),
            tab_accent: cfg.theme.tab_accent_quad(),
            tab_active_fg: cfg.theme.tab_active_fg_glyphon(),
            tab_inactive_fg: cfg.theme.tab_inactive_fg_glyphon(),
            tab_close_fg: cfg.theme.tab_close_fg_glyphon(),
            last_tab_text: String::new(),
            measured_advance: None,
            bg_pipeline,
            bg_buffer,
        };
        // Measure the real advance up front so the first grid estimate
        // already matches the font (and any configured family).
        renderer.measured_advance = renderer.probe_advance();
        renderer
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.surface_config.width = width;
        self.surface_config.height = height;
        self.surface.configure(&self.device, &self.surface_config);
        // Force a re-shape next frame (scale may have changed).
        self.last_tab_text.clear();
        self.pane_rows.clear();
        self.window.request_redraw();
    }

    pub fn apply_config(&mut self, cfg: &NeotermConfig) {
        self.bg = cfg.bg_wgpu();
        self.flash_bg = flash_color(cfg);
        self.fg = cfg.fg_glyphon();
        self.cursor_rgb = cursor_color(cfg);
        self.bg_quad = bg_color(cfg);
        self.sel_bg = cfg.theme.selection_quad();
        self.track_bg = cfg.theme.scrollbar_track_quad();
        self.thumb_bg = cfg.theme.scrollbar_thumb_quad();
        self.strip_bg = cfg.theme.tab_bar_bg_quad();
        self.tab_accent = cfg.theme.tab_accent_quad();
        self.tab_active_fg = cfg.theme.tab_active_fg_glyphon();
        self.tab_inactive_fg = cfg.theme.tab_inactive_fg_glyphon();
        self.tab_close_fg = cfg.theme.tab_close_fg_glyphon();
        // Family is resolved against the installed fonts; a missing family
        // falls back to generic monospace instead of a proportional face.
        let family = resolve_family(&mut self.font_system, &cfg.font_family);
        let family_changed = family != self.font_family;
        self.font_family = family;
        // Any config change forces re-shape (colors/font may differ).
        self.last_tab_text.clear();
        self.pane_rows.clear();
        if family_changed || (cfg.font_size - self.font_size).abs() > f32::EPSILON {
            self.font_size = cfg.font_size;
            let m = Metrics::new(self.font_size, self.font_size * LINE_HEIGHT);
            for bufs in self.pane_buffers.values_mut() {
                for buf in bufs {
                    buf.set_metrics(m);
                }
            }
            let m2 = Metrics::new(self.font_size, self.font_size * 1.3);
            self.tab_buffer.set_metrics(m2);
            self.measured_advance = self.probe_advance();
        }
    }

    /// Visual bell: flash the body background for one frame.
    pub fn set_flash(&mut self, on: bool) {
        self.flash = on;
    }

    /// Runtime font zoom (session-only): retargets every text buffer and
    /// re-measures the advance so grid/hit-testing follow the new face.
    pub fn set_font_size(&mut self, size: f32) {
        let size = size.clamp(6.0, 48.0);
        if (size - self.font_size).abs() < 0.01 {
            return;
        }
        self.font_size = size;
        let m = Metrics::new(size, size * LINE_HEIGHT);
        for bufs in self.pane_buffers.values_mut() {
            for buf in bufs {
                buf.set_metrics(m);
            }
        }
        self.tab_buffer.set_metrics(Metrics::new(size, size * 1.3));
        self.last_tab_text.clear();
        self.pane_rows.clear();
        self.measured_advance = self.probe_advance();
        log::info!("font size: {size:.1}pt");
        self.window.request_redraw();
    }

    pub fn font_size(&self) -> f32 {
        self.font_size
    }

    /// Shape a throwaway run to get the real advance for the current face.
    /// Falls back to `None` (0.62em heuristic) only if shaping yields one
    /// glyph or less.
    fn probe_advance(&mut self) -> Option<f32> {
        let family = match self.font_family.as_deref() {
            Some(name) => Family::Name(name),
            None => Family::Monospace,
        };
        let metrics = Metrics::new(self.font_size, self.font_size * LINE_HEIGHT);
        let mut buf = Buffer::new(&mut self.font_system, metrics);
        buf.set_size(Some(4000.0), Some(metrics.line_height));
        let attrs = Attrs::new().family(family).color(Color::rgb(255, 255, 255));
        buf.set_text("MMMMMMMMMMMMMMMMMMMM", &attrs, Shaping::Advanced, None);
        buf.shape_until_scroll(&mut self.font_system, false);
        for run in buf.layout_runs() {
            if run.glyphs.len() >= 2 {
                let first = run.glyphs[0].x;
                let last = run.glyphs[run.glyphs.len() - 1].x;
                let adv = (last - first) / (run.glyphs.len() - 1) as f32;
                if adv > 0.0 {
                    return Some(adv);
                }
            }
        }
        None
    }

    /// Line height in physical pixels at the current scale.
    fn line_height(&self, scale: f32) -> f32 {
        self.font_size * LINE_HEIGHT * scale
    }

    /// Height of the tab strip in layout units: grows with the font so
    /// labels and the active-tab accent never clip at large zoom levels.
    fn strip_height(&self) -> f32 {
        (self.font_size * 1.3 + 16.0).max(STRIP_MIN_H)
    }

    /// Monospace advance in layout units: measured from shaped glyphs when
    /// available, otherwise the 0.62em heuristic.
    fn advance(&self) -> f32 {
        self.measured_advance.unwrap_or(self.font_size * 0.62)
    }

    /// Terminal body geometry in physical pixels: (cell_w, line_h, left, top).
    /// Must stay in sync with the TextArea placement in `draw`.
    /// `strip` = tab strip visible: hidden strips give the body the full
    /// window minus a small top padding.
    pub fn body_geometry(&self, scale: f32, strip: bool) -> (f32, f32, f32, f32) {
        (
            self.advance() * scale,
            self.line_height(scale),
            PAD_X * scale,
            if strip {
                self.strip_height() * scale
            } else {
                PAD_TOP * scale
            },
        )
    }

    /// Grid cells fitting in a window, using the same geometry as drawing
    /// and hit-testing. This is the single source of truth for reflows
    /// (window resize, font zoom, strip show/hide).
    pub fn grid_size(
        &self,
        width_px: u32,
        height_px: u32,
        scale: f32,
        strip: bool,
    ) -> (u16, u16) {
        let (cell_w, line_h, left, top) = self.body_geometry(scale, strip);
        let cols = ((width_px as f32 - 2.0 * left) / cell_w.max(1.0)).floor();
        let rows = ((height_px as f32 - top - PAD_BOTTOM * scale) / line_h.max(1.0)).floor();
        (
            (cols.max(20.0) as u16).min(400),
            (rows.max(5.0) as u16).min(200),
        )
    }

    /// Build a cosmic-text attrs list for one row: default fg + per-span
    /// overrides (color, bold, italic) in the configured family.
    fn row_attrs(defaults: &Attrs, family: Family<'_>, row: &RowContent) -> AttrsList {
        let mut list = AttrsList::new(defaults);
        for (range, style) in &row.spans {
            let (r, g, b) = style.fg;
            let mut attrs = Attrs::new().family(family).color(Color::rgb(r, g, b));
            if style.bold {
                attrs = attrs.weight(Weight::BOLD);
            }
            if style.italic {
                attrs = attrs.style(Style::Italic);
            }
            list.add_span(range.clone(), &attrs);
        }
        list
    }

    pub fn draw(
        &mut self,
        titles: &[String],
        active: usize,
        panes: &[PaneDraw],
        dividers: &[CellRect],
        scroll: ScrollState,
        strip: bool,
    ) {
        let scale = self.window.scale_factor() as f32;
        let w = self.surface_config.width;
        let h = self.surface_config.height;

        self.viewport.update(
            &self.queue,
            Resolution {
                width: w,
                height: h,
            },
        );

        // Tab bar text (single source of truth shared with hit-testing),
        // styled in pieces: active segment bold + accent, close marks and
        // the `+` mark muted, everything else dimmed inactive.
        let family = match self.font_family.as_deref() {
            Some(name) => Family::Name(name),
            None => Family::Monospace,
        };
        let mut tab_text = mouse::tab_bar_text(titles, active);
        if tab_text.is_empty() {
            tab_text = " NeoTerm ".to_string();
        }
        if tab_text != self.last_tab_text {
            let (active_range, x_ranges, plus_range) = mouse::tab_span_ranges(titles, active);
            let base = Attrs::new().family(family).color(self.tab_inactive_fg);
            let active_attrs = Attrs::new()
                .family(family)
                .color(self.tab_active_fg)
                .weight(Weight::BOLD);
            let close_attrs = Attrs::new().family(family).color(self.tab_close_fg);
            // Partition the text at every span edge; each piece takes the
            // innermost style (close mark wins inside the active segment).
            let mut cuts = vec![
                0,
                tab_text.len(),
                active_range.start,
                active_range.end,
                plus_range.start,
                plus_range.end,
            ];
            for xr in &x_ranges {
                cuts.push(xr.start);
                cuts.push(xr.end);
            }
            cuts.sort_unstable();
            cuts.dedup();
            let spans: Vec<(&str, Attrs)> = cuts
                .windows(2)
                .filter_map(|w| {
                    let (a, b) = (w[0], w[1]);
                    if a >= b {
                        return None;
                    }
                    let piece = tab_text.get(a..b)?;
                    let in_active = active_range.start < active_range.end
                        && a >= active_range.start
                        && b <= active_range.end;
                    let is_x = x_ranges.iter().any(|xr| a >= xr.start && b <= xr.end);
                    let is_plus = plus_range.start < plus_range.end
                        && a >= plus_range.start
                        && b <= plus_range.end;
                    let attrs = if is_x || is_plus {
                        close_attrs.clone()
                    } else if in_active {
                        active_attrs.clone()
                    } else {
                        base.clone()
                    };
                    Some((piece, attrs))
                })
                .collect();
            self.tab_buffer.set_rich_text(spans, &base, Shaping::Advanced, None);
            self.tab_buffer.shape_until_scroll(&mut self.font_system, false);
            self.last_tab_text = tab_text;
        }

        // Per-pane row buffers: reshape only rows whose content changed
        // (text, spans, selection range, or background). Caches are keyed
        // by pane id and pruned when panes close.
        self.pane_buffers.retain(|id, _| panes.iter().any(|p| p.id == *id));
        self.pane_rows.retain(|id, _| panes.iter().any(|p| p.id == *id));
        let defaults = Attrs::new().family(family).color(self.fg);
        let m = Metrics::new(self.font_size, self.font_size * LINE_HEIGHT);
        for pane in panes {
            let bufs = self.pane_buffers.entry(pane.id).or_default();
            while bufs.len() < pane.rows.len() {
                let mut buf = Buffer::new(&mut self.font_system, m);
                buf.set_size(Some(4000.0), Some(m.line_height));
                bufs.push(buf);
            }
            bufs.truncate(pane.rows.len());
            let cached = self.pane_rows.entry(pane.id).or_default();
            if cached.len() != pane.rows.len() {
                *cached = vec![RowContent::empty(); pane.rows.len()];
            }
            for (i, row) in pane.rows.iter().enumerate() {
                if cached[i] == *row {
                    continue;
                }
                let attrs = Self::row_attrs(&defaults, family, row);
                let buf = &mut bufs[i];
                if buf.lines.is_empty() {
                    buf.lines.push(BufferLine::new(
                        row.text.clone(),
                        LineEnding::None,
                        attrs,
                        Shaping::Advanced,
                    ));
                } else {
                    buf.lines[0].set_text(row.text.clone(), LineEnding::None, attrs);
                    buf.lines.truncate(1);
                }
                buf.shape_until_scroll(&mut self.font_system, false);
                cached[i] = row.clone();
            }
        }

        // Measure the real monospace advance from shaped glyphs (drives
        // mouse->cell mapping). Falls back to the heuristic until a row
        // with at least two glyphs has been shaped.
        if self.measured_advance.is_none() {
            'outer: for bufs in self.pane_buffers.values() {
                for buf in bufs {
                    for run in buf.layout_runs() {
                        if run.glyphs.len() >= 2 {
                            let first = run.glyphs[0].x;
                            let last = run.glyphs[run.glyphs.len() - 1].x;
                            let adv = (last - first) / (run.glyphs.len() - 1) as f32;
                            if adv > 0.0 {
                                self.measured_advance = Some(adv);
                                break 'outer;
                            }
                        }
                    }
                }
            }
        }

        let (cell_w, line_h, body_left, body_top) = self.body_geometry(scale, strip);
        // Center the strip label in its band; grows with the font.
        let tab_top = ((body_top - self.font_size * 1.3 * scale) / 2.0).max(2.0);
        let tab_left = TAB_LEFT * scale;

        // Background rects in physical pixels, drawn under the text in order:
        // tab strip first (skipped when hidden), then per-cell backgrounds,
        // selection, scrollbar.
        let mut bg_verts: Vec<SelVertex> = Vec::new();
        // Seamless Ghostty-style strip: full-width background in the body
        // color (or the explicit override). No pill, no separator — the
        // active tab reads through bold accent text and a short underline.
        if strip {
            push_quad(
                &mut bg_verts,
                0.0,
                0.0,
                w as f32,
                body_top,
                w as f32,
                h as f32,
                with_alpha(self.strip_bg, 1.0),
            );
            // Accent underline under the active tab (theme `tab_accent`).
            if !titles.is_empty() && active < titles.len() {
                let regions = mouse::tab_regions(titles, active, self.advance(), tab_left, scale);
                if let Some(&(x0, x1)) = regions.get(active) {
                    let inset = 6.0 * scale;
                    push_quad(
                        &mut bg_verts,
                        x0 + inset,
                        body_top - 3.0 * scale,
                        (x1 - inset).max(x0 + inset),
                        body_top - 1.0 * scale,
                        w as f32,
                        h as f32,
                        with_alpha(self.tab_accent, 0.95),
                    );
                }
            }
        }
        // Dividers own a full grid cell for layout/hit-testing, but are
        // painted as a hairline centered in that cell: the gap reads as a
        // seam, not a painted bar.
        let seam = (1.0 * scale).max(1.0);
        for d in dividers {
            let x0 = body_left + d.col as f32 * cell_w;
            let y0 = body_top + d.row as f32 * line_h;
            let cw = d.cols.max(1) as f32 * cell_w;
            let ch = d.rows.max(1) as f32 * line_h;
            let (sx0, sx1, sy0, sy1) = if d.cols <= d.rows {
                // Vertical seam (1 column wide).
                let x = x0 + (cw - seam) / 2.0;
                (x, x + seam, y0, y0 + ch)
            } else {
                // Horizontal seam (1 row tall).
                let y = y0 + (ch - seam) / 2.0;
                (x0, x0 + cw, y, y + seam)
            };
            push_quad(
                &mut bg_verts,
                sx0,
                sy0,
                sx1,
                sy1,
                w as f32,
                h as f32,
                with_alpha(self.thumb_bg, 0.7),
            );
        }
        for pane in panes {
            let ox = body_left + pane.rect.col as f32 * cell_w;
            let oy = body_top + pane.rect.row as f32 * line_h;
            // Cursor block: drawn before the text, so the glyph sits on top
            // of it (inverse video) and stays readable.
            if let Some((row, col)) = pane.cursor {
                let x0 = ox + col as f32 * cell_w;
                let y0 = oy + row as f32 * line_h;
                push_quad(
                    &mut bg_verts,
                    x0,
                    y0,
                    x0 + cell_w,
                    y0 + line_h,
                    w as f32,
                    h as f32,
                    with_alpha(self.cursor_rgb, 1.0),
                );
            }
            for (i, row) in pane.rows.iter().enumerate() {
                let y0 = oy + i as f32 * line_h;
                let y1 = y0 + line_h;
                for (lo, hi, (r, g, b)) in &row.bg_rects {
                    push_quad(
                        &mut bg_verts,
                        ox + *lo as f32 * cell_w,
                        y0,
                        ox + (*hi + 1) as f32 * cell_w,
                        y1,
                        w as f32,
                        h as f32,
                        [
                            crate::config::NeotermConfig::srgb8_to_linear(*r),
                            crate::config::NeotermConfig::srgb8_to_linear(*g),
                            crate::config::NeotermConfig::srgb8_to_linear(*b),
                            1.0,
                        ],
                    );
                }
            }
        }
        for pane in panes {
            let ox = body_left + pane.rect.col as f32 * cell_w;
            let oy = body_top + pane.rect.row as f32 * line_h;
            for (i, row) in pane.rows.iter().enumerate() {
                let Some((lo, hi)) = row.sel_cols else {
                    continue;
                };
                let x0 = ox + lo as f32 * cell_w;
                let x1 = ox + (hi + 1) as f32 * cell_w;
                let y0 = oy + i as f32 * line_h;
                let y1 = y0 + line_h;
                push_quad(
                    &mut bg_verts,
                    x0,
                    y0,
                    x1,
                    y1,
                    w as f32,
                    h as f32,
                    with_alpha(self.sel_bg, 0.72),
                );
            }
        }
        // Scrollbar: hidden without history. Thumb at bottom = live view.
        // The track lives inside the active pane (never over a neighbor).
        let bar_w = mouse::BAR_W * scale;
        let (track_top, track_h, track_x) = match scroll.pane {
            Some(r) => (
                body_top + r.row as f32 * line_h,
                (r.rows as f32 * line_h - 4.0 * scale).max(0.0),
                body_left + (r.col + r.cols) as f32 * cell_w - bar_w - mouse::BAR_MARGIN * scale,
            ),
            None => (
                body_top,
                (h as f32 - body_top - 4.0 * scale).max(0.0),
                w as f32 - bar_w - mouse::BAR_MARGIN * scale,
            ),
        };
        let sbar = mouse::scrollbar(track_top, track_h, scale, scroll.history, scroll.offset, scroll.rows);
        if let Some((thumb_y, thumb_h)) = sbar.thumb {
            push_quad(
                &mut bg_verts,
                track_x,
                track_top,
                track_x + bar_w,
                track_top + track_h,
                w as f32,
                h as f32,
                with_alpha(self.track_bg, 0.35),
            );
            push_quad(
                &mut bg_verts,
                track_x,
                thumb_y,
                track_x + bar_w,
                thumb_y + thumb_h,
                w as f32,
                h as f32,
                with_alpha(self.thumb_bg, 0.85),
            );
        }
        // Focus treatment (only meaningful with more than one pane):
        // the focused pane gets a hairline outline, the others recede
        // behind a scrim drawn over their text in the last pass.
        let mut scrim_verts: Vec<SelVertex> = Vec::new();
        if panes.len() > 1 {
            let hair = scale.max(1.0);
            for pane in panes {
                let x0 = body_left + pane.rect.col as f32 * cell_w;
                let y0 = body_top + pane.rect.row as f32 * line_h;
                let x1 = x0 + pane.rect.cols as f32 * cell_w;
                let y1 = y0 + pane.rect.rows as f32 * line_h;
                if pane.focused {
                    // Outline, inset by one pixel so it never touches text.
                    let a = with_alpha(self.tab_accent, 0.38);
                    for (rx0, ry0, rx1, ry1) in [
                        (x0 + hair, y0 + hair, x1 - hair, y0 + hair * 2.0),
                        (x0 + hair, y1 - hair * 2.0, x1 - hair, y1 - hair),
                        (x0 + hair, y0 + hair, x0 + hair * 2.0, y1 - hair),
                        (x1 - hair * 2.0, y0 + hair, x1 - hair, y1 - hair),
                    ] {
                        push_quad(
                            &mut bg_verts,
                            rx0,
                            ry0,
                            rx1,
                            ry1,
                            w as f32,
                            h as f32,
                            a,
                        );
                    }
                } else {
                    // Scrim: the pane fades toward the background.
                    push_quad(
                        &mut scrim_verts,
                        x0,
                        y0,
                        x1,
                        y1,
                        w as f32,
                        h as f32,
                        with_alpha(self.bg_quad, 0.38),
                    );
                }
            }
        }

        // Upload capped at the preallocated vertex buffer.
        bg_verts.truncate(MAX_BG_RECTS * SEL_VERTS_PER_RECT);
        let bg_count = bg_verts.len() as u32;
        let scrim_start = bg_count;
        let scrim_count = scrim_verts.len() as u32;
        bg_verts.extend_from_slice(&scrim_verts);
        if bg_count + scrim_count > 0 {
            self.queue.write_buffer(
                &self.bg_buffer,
                0,
                bytemuck::cast_slice(&bg_verts),
            );
        }

        // One text area per pane row, clipped to the pane rect so wide
        // glyphs never bleed across dividers.
        let mut total_rows = 0;
        for pane in panes {
            total_rows += pane.rows.len();
        }
        let mut areas = Vec::with_capacity(total_rows + 1);
        if strip {
            areas.push(TextArea {
                buffer: &self.tab_buffer,
                left: tab_left,
                top: tab_top,
                scale,
                bounds: TextBounds {
                    left: 0,
                    top: 0,
                    right: w as i32,
                    bottom: (body_top as i32).max(10),
                },
                default_color: self.tab_inactive_fg,
                custom_glyphs: &[],
            });
        }
        for pane in panes {
            let bufs = self.pane_buffers.get(&pane.id);
            let Some(bufs) = bufs else {
                continue;
            };
            let px0 = body_left + pane.rect.col as f32 * cell_w;
            let py0 = body_top + pane.rect.row as f32 * line_h;
            let px1 = px0 + pane.rect.cols as f32 * cell_w;
            let py1 = py0 + pane.rect.rows as f32 * line_h;
            for (i, buf) in bufs.iter().enumerate() {
                let top = py0 + i as f32 * line_h;
                areas.push(TextArea {
                    buffer: buf,
                    left: px0,
                    top,
                    scale,
                    bounds: TextBounds {
                        left: px0 as i32,
                        top: py0 as i32,
                        right: px1 as i32,
                        bottom: py1 as i32,
                    },
                    default_color: self.fg,
                    custom_glyphs: &[],
                });
            }
        }

        if self
            .text_renderer
            .prepare(
                &self.device,
                &self.queue,
                &mut self.font_system,
                &mut self.atlas,
                &self.viewport,
                areas,
                &mut self.swash_cache,
            )
            .is_err()
        {
            return;
        }

        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f) => f,
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                self.window.request_redraw();
                return;
            }
            wgpu::CurrentSurfaceTexture::Outdated
            | wgpu::CurrentSurfaceTexture::Suboptimal(_) => {
                self.surface.configure(&self.device, &self.surface_config);
                self.window.request_redraw();
                return;
            }
            wgpu::CurrentSurfaceTexture::Lost => {
                let surf = self.instance.create_surface(self.window.clone()).unwrap();
                self.surface = surf;
                self.surface.configure(&self.device, &self.surface_config);
                self.window.request_redraw();
                return;
            }
            wgpu::CurrentSurfaceTexture::Validation => return,
        };
        let view = frame.texture.create_view(&TextureViewDescriptor::default());
        let mut encoder =
            self.device
                .create_command_encoder(&CommandEncoderDescriptor { label: None });
        {
            // Pass 1: clear + backgrounds (cell bg, selection, scrollbar).
            let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
                label: Some("neoterm-bg"),
                color_attachments: &[Some(RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: Operations {
                        load: LoadOp::Clear(if self.flash { self.flash_bg } else { self.bg }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            if bg_count > 0 {
                pass.set_pipeline(&self.bg_pipeline);
                pass.set_vertex_buffer(0, self.bg_buffer.slice(..));
                pass.draw(0..bg_count, 0..1);
            }
        }
        {
            // Pass 2: text over the backgrounds.
            let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
                label: Some("neoterm-text"),
                color_attachments: &[Some(RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: Operations {
                        load: LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            let _ = self.text_renderer.render(&self.atlas, &self.viewport, &mut pass);
        }
        if scrim_count > 0 {
            // Pass 3: unfocused panes recede behind a scrim.
            let mut pass = encoder.begin_render_pass(&RenderPassDescriptor {
                label: Some("neoterm-scrim"),
                color_attachments: &[Some(RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: Operations {
                        load: LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.bg_pipeline);
            pass.set_vertex_buffer(0, self.bg_buffer.slice(..));
            pass.draw(scrim_start..scrim_start + scrim_count, 0..1);
        }
        self.queue.submit(Some(encoder.finish()));
        self.queue.present(frame);
        self.atlas.trim();
    }

    pub fn window_logical_size() -> LogicalSize<f64> {
        LogicalSize::new(960.0, 600.0)
    }
}

/// Resolve a comma-separated family preference list to the first installed
/// family. `None` means the generic `Family::Monospace` (also the fallback
/// when nothing in the list is installed, so a typo never silently turns
/// the terminal proportional).
fn resolve_family(font_system: &mut FontSystem, spec: &str) -> Option<String> {
    let spec = spec.trim();
    if spec.is_empty() || spec.eq_ignore_ascii_case("monospace") {
        return None;
    }
    // Family name -> any installed face monospaced?
    let mut installed: Vec<(String, bool)> = Vec::new();
    for face in font_system.db_mut().faces() {
        for (name, _) in &face.families {
            installed.push((name.clone(), face.monospaced));
        }
    }
    for name in spec.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        if name.eq_ignore_ascii_case("monospace") {
            return None;
        }
        let hit = installed
            .iter()
            .find(|(f, _)| f.eq_ignore_ascii_case(name))
            .map(|(_, mono)| *mono);
        if let Some(mono) = hit {
            if !mono {
                log::warn!("font family {name:?} is not monospaced; the cell grid may drift");
            }
            log::info!("font family: {name:?}");
            return Some(name.to_string());
        }
    }
    log::warn!("font_family {spec:?} not installed; using monospace");
    None
}

/// RGBA quad color from a theme RGB triplet.
fn with_alpha(col: [f32; 3], a: f32) -> [f32; 4] {
    [col[0], col[1], col[2], a]
}

/// Visual-bell clear color: the body background tinted toward the cursor
/// color. Built in linear space (the swapchain is an sRGB target).
fn flash_color(cfg: &NeotermConfig) -> wgpu::Color {
    let bg = cfg.bg_wgpu();
    let (r, g, b) = cfg.theme.cursor_rgb();
    let mix = |base: f64, to: u8| {
        let to = NeotermConfig::srgb8_to_linear(to) as f64;
        base + (to - base) * 0.22
    };
    wgpu::Color {
        r: mix(bg.r, r),
        g: mix(bg.g, g),
        b: mix(bg.b, b),
        a: 1.0,
    }
}

/// Body background as a linear RGB triple (the swapchain is an sRGB target).
fn bg_color(cfg: &NeotermConfig) -> [f32; 3] {
    let (r, g, b) = cfg.theme.background_rgb();
    [
        NeotermConfig::srgb8_to_linear(r),
        NeotermConfig::srgb8_to_linear(g),
        NeotermConfig::srgb8_to_linear(b),
    ]
}

/// Cursor block color in linear RGB (the swapchain is an sRGB target).
fn cursor_color(cfg: &NeotermConfig) -> [f32; 3] {
    let (r, g, b) = cfg.theme.cursor_rgb();
    [
        NeotermConfig::srgb8_to_linear(r),
        NeotermConfig::srgb8_to_linear(g),
        NeotermConfig::srgb8_to_linear(b),
    ]
}

/// Append a solid-color quad (two triangles) in NDC coordinates.
/// Physical pixels in, y-down; NDC y-up out. Degenerate rects are skipped.
fn push_quad(
    out: &mut Vec<SelVertex>,
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    w: f32,
    h: f32,
    col: [f32; 4],
) {
    if w <= 0.0 || h <= 0.0 || x1 <= x0 || y1 <= y0 {
        return;
    }
    let nx = |x: f32| x / w * 2.0 - 1.0;
    let ny = |y: f32| 1.0 - y / h * 2.0;
    let (ax, ay) = (nx(x0), ny(y0));
    let (bx, by) = (nx(x1), ny(y1));
    // Triangle 1: TL, TR, BR. Triangle 2: TL, BR, BL.
    for (px, py) in [
        (ax, ay),
        (bx, ay),
        (bx, by),
        (ax, ay),
        (bx, by),
        (ax, by),
    ] {
        out.push(SelVertex {
            pos: [px, py],
            col,
        });
    }
}
