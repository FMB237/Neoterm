//! Graphics protocol support (Sixel, Kitty graphics)
//!
//! This module handles parsing and rendering of graphics protocols
//! using only the standard library and existing dependencies.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Graphics protocol type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphicsProtocol {
    Sixel,
    Kitty,
}

/// A single graphics image placed at a grid position
#[derive(Debug, Clone)]
pub struct GraphicsImage {
    pub protocol: GraphicsProtocol,
    pub id: u32,
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
    pub pixel_data: Vec<u8>, // RGBA
    pub z_index: i32,
}

/// Graphics state for a pane
#[derive(Debug, Default)]
pub struct GraphicsState {
    images: HashMap<u32, GraphicsImage>,
    next_id: u32,
    // Sixel decoder state
    sixel_decoder: Option<SixelDecoder>,
    // Kitty graphics decoder state
    kitty_decoder: Option<KittyDecoder>,
}

impl GraphicsState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a graphics image
    pub fn add_image(&mut self, mut image: GraphicsImage) -> u32 {
        if image.id == 0 {
            image.id = self.next_id;
            self.next_id += 1;
        }
        let id = image.id;
        self.images.insert(id, image);
        id
    }

    /// Remove a graphics image by ID
    pub fn remove_image(&mut self, id: u32) -> Option<GraphicsImage> {
        self.images.remove(&id)
    }

    /// Clear all graphics images
    pub fn clear(&mut self) {
        self.images.clear();
    }

    /// Get all images sorted by z-index (for rendering)
    pub fn images_sorted(&self) -> Vec<&GraphicsImage> {
        let mut images: Vec<_> = self.images.values().collect();
        images.sort_by_key(|img| img.z_index);
        images
    }

    /// Get mutable reference to sixel decoder
    pub fn sixel_decoder(&mut self) -> &mut Option<SixelDecoder> {
        &mut self.sixel_decoder
    }

    /// Get mutable reference to sixel decoder (mut alias for preprocessing)
    pub fn sixel_decoder_mut(&mut self) -> &mut Option<SixelDecoder> {
        &mut self.sixel_decoder
    }

    /// Get mutable reference to kitty decoder
    pub fn kitty_decoder(&mut self) -> &mut Option<KittyDecoder> {
        &mut self.kitty_decoder
    }

    /// Get mutable reference to kitty decoder (mut alias for preprocessing)
    pub fn kitty_decoder_mut(&mut self) -> &mut Option<KittyDecoder> {
        &mut self.kitty_decoder
    }
}

/// Sixel decoder state
#[derive(Debug, Default)]
pub struct SixelDecoder {
    buffer: Vec<u8>,
    width: u16,
    height: u16,
    color_registers: Vec<[u8; 3]>,
    current_color: usize,
    palette_mode: bool,
}

impl SixelDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Process a chunk of sixel data
    pub fn process(&mut self, data: &[u8]) {
        self.buffer.extend_from_slice(data);
    }

    /// Finalize and decode the sixel image
    pub fn finalize(&mut self) -> Option<GraphicsImage> {
        if self.buffer.is_empty() {
            return None;
        }

        // Basic sixel decoding - simplified implementation
        // Full sixel decoding is complex; this is a minimal implementation
        let image = self.decode_sixel()?;
        self.buffer.clear();
        self.color_registers.clear();
        Some(image)
    }

    fn decode_sixel(&mut self) -> Option<GraphicsImage> {
        // Simplified sixel decoder - converts sixel to RGBA
        // This is a placeholder implementation
        // Real implementation would parse sixel format properly
        let width = self.width.max(1);
        let height = self.height.max(1);
        let mut pixel_data = vec![0u8; (width * height * 4) as usize];

        // Fill with a placeholder pattern
        for y in 0..height {
            for x in 0..width {
                let idx = ((y * width + x) * 4) as usize;
                pixel_data[idx] = (x * 255 / width) as u8;     // R
                pixel_data[idx + 1] = (y * 255 / height) as u8; // G
                pixel_data[idx + 2] = 128;                      // B
                pixel_data[idx + 3] = 255;                      // A
            }
        }

        Some(GraphicsImage {
            protocol: GraphicsProtocol::Sixel,
            id: 0,
            x: 0,
            y: 0,
            width,
            height,
            pixel_data,
            z_index: 0,
        })
    }
}

/// Kitty graphics protocol decoder state
#[derive(Debug, Default)]
pub struct KittyDecoder {
    buffer: Vec<u8>,
    current_image: Option<KittyImageState>,
}

/// Kitty graphics image being built
#[derive(Debug, Clone)]
struct KittyImageState {
    id: u32,
    width: u16,
    height: u16,
    format: u8,
    compression: u8,
    data: Vec<u8>,
    done: bool,
}

impl KittyDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Process a kitty graphics escape sequence
    pub fn process(&mut self, params: &[&[u8]], data: &[u8]) -> Option<GraphicsImage> {
        if params.is_empty() {
            return None;
        }

        // Parse first parameter (action)
        let action = std::str::from_utf8(params[0]).ok()?;
        let action: u8 = action.parse().ok()?;

        match action {
            0 => self.process_transmit(params, data),      // Transmit image data
            1 => self.process_delete(params),              // Delete image
            2 => self.process_query(params),               // Query image
            _ => None,
        }
    }

    fn process_transmit(&mut self, params: &[&[u8]], data: &[u8]) -> Option<GraphicsImage> {
        if params.len() < 7 {
            return None;
        }

        let id = parse_u32(params[1])?;
        let width = parse_u16(params[2])?;
        let height = parse_u16(params[3])?;
        let format = parse_u8(params[4])?;
        let compression = parse_u8(params[5])?;
        let more = parse_u8(params[6])? != 0;

        if self.current_image.is_none() || self.current_image.as_ref().unwrap().id != id {
            self.current_image = Some(KittyImageState {
                id,
                width,
                height,
                format,
                compression,
                data: Vec::new(),
                done: false,
            });
        }

        if let Some(img) = &mut self.current_image {
            img.data.extend_from_slice(data);
            img.done = !more;

            if img.done {
                // Clone the state to avoid borrow checker issues
                let state = img.clone();
                self.current_image = None;
                return self.decode_kitty(&state);
            }
        }

        None
    }

    fn process_delete(&mut self, params: &[&[u8]]) -> Option<GraphicsImage> {
        // Delete action - not returning an image
        None
    }

    fn process_query(&mut self, params: &[&[u8]]) -> Option<GraphicsImage> {
        // Query action - not returning an image
        None
    }

    fn decode_kitty(&self, state: &KittyImageState) -> Option<GraphicsImage> {
        // Simplified kitty graphics decoder
        // Real implementation would handle different formats (PNG, RGBA, etc.)
        // and compression (zlib, none)
        let width = state.width.max(1);
        let height = state.height.max(1);
        let mut pixel_data = vec![0u8; (width * height * 4) as usize];

        // If we have raw RGBA data, use it (format 24 = RGBA)
        if state.format == 24 && state.data.len() >= (width * height * 4) as usize {
            pixel_data.copy_from_slice(&state.data[..(width * height * 4) as usize]);
        } else {
            // Placeholder pattern
            for y in 0..height {
                for x in 0..width {
                    let idx = ((y * width + x) * 4) as usize;
                    pixel_data[idx] = (x * 255 / width) as u8;
                    pixel_data[idx + 1] = (y * 255 / height) as u8;
                    pixel_data[idx + 2] = 200;
                    pixel_data[idx + 3] = 255;
                }
            }
        }

        Some(GraphicsImage {
            protocol: GraphicsProtocol::Kitty,
            id: state.id,
            x: 0,
            y: 0,
            width,
            height,
            pixel_data,
            z_index: 0,
        })
    }
}

fn parse_u32(bytes: &[u8]) -> Option<u32> {
    std::str::from_utf8(bytes).ok()?.parse().ok()
}

fn parse_u16(bytes: &[u8]) -> Option<u16> {
    std::str::from_utf8(bytes).ok()?.parse().ok()
}

fn parse_u8(bytes: &[u8]) -> Option<u8> {
    std::str::from_utf8(bytes).ok()?.parse().ok()
}

/// Thread-safe graphics state for sharing between PTY thread and render thread
pub type SharedGraphicsState = Arc<Mutex<GraphicsState>>;

/// Create a new shared graphics state
pub fn new_shared_graphics_state() -> SharedGraphicsState {
    Arc::new(Mutex::new(GraphicsState::new()))
}

/// Pre-process PTY output to handle graphics escape sequences.
///
/// This scans the input bytes for graphics protocols (Sixel, Kitty) and
/// delegates to the graphics state. Returns the remaining bytes that
/// should be passed to the regular vte processor.
pub fn preprocess_graphics(bytes: &[u8], graphics: &SharedGraphicsState) -> Vec<u8> {
    let mut output = Vec::with_capacity(bytes.len());
    let mut i = 0;
    
    while i < bytes.len() {
        // Check for ESC (0x1B) - start of escape sequences
        if bytes[i] == 0x1B && i + 1 < bytes.len() {
            match bytes[i + 1] {
                // DCS (Device Control String) - 0x50 'P'
                // Sixel uses: ESC P q ... ESC \
                0x50 => {
                    if let Some((consumed, remaining)) = handle_dcs(&bytes[i..], graphics) {
                        i += consumed;
                        output.extend_from_slice(&remaining);
                        continue;
                    }
                }
                // OSC (Operating System Command) - 0x5D ']'
                // Kitty graphics uses: ESC ] G ... ESC \ or ESC ] 1337 ... ESC \
                0x5D => {
                    if let Some((consumed, remaining)) = handle_osc(&bytes[i..], graphics) {
                        i += consumed;
                        output.extend_from_slice(&remaining);
                        continue;
                    }
                }
                // CSI (Control Sequence Introducer) - 0x5B '['
                // Could have graphics-related sequences
                0x5B => {
                    // Pass through CSI sequences normally
                }
                _ => {}
            }
        }
        output.push(bytes[i]);
        i += 1;
    }
    
    output
}

/// Handle DCS (Device Control String) sequences for Sixel
fn handle_dcs(bytes: &[u8], graphics: &SharedGraphicsState) -> Option<(usize, Vec<u8>)> {
    // Minimum: ESC P q ... ESC \  (at least 6 bytes)
    if bytes.len() < 6 || bytes[0] != 0x1B || bytes[1] != 0x50 {
        return None;
    }
    
    // Find the end of DCS (ST = ESC \ = 0x1B 0x5C, or 0x9C)
    let mut i = 2;
    let mut params_end = None;
    let mut in_param = false;
    
    // DCS format: ESC P <params> <intermediates> <final> <data> ESC \
    // For sixel: ESC P q <data> ESC \
    // The 'q' is the final character indicating sixel
    
    // Skip params and intermediates to find final character
    while i < bytes.len() {
        let b = bytes[i];
        if b == 0x1B && i + 1 < bytes.len() && bytes[i + 1] == 0x5C {
            // Found ESC \ (ST)
            break;
        }
        if b == 0x9C {
            // Found single-byte ST
            break;
        }
        // Check for final character (0x40-0x7E) after params/intermediates
        if !in_param && b >= 0x40 && b <= 0x7E {
            params_end = Some(i);
        }
        if b >= 0x30 && b <= 0x3F {
            in_param = true;
        } else if b >= 0x20 && b <= 0x2F {
            in_param = false;
        } else if b >= 0x40 && b <= 0x7E {
            in_param = false;
        }
        i += 1;
    }
    
    if i >= bytes.len() {
        return None; // Incomplete sequence
    }
    
    let end = i;
    let st_len = if bytes[i] == 0x1B { 2 } else { 1 };
    
    // Check if this is a sixel sequence (final char 'q' = 0x71)
    if let Some(final_pos) = params_end {
        if bytes[final_pos] == 0x71 { // 'q'
            // This is a sixel sequence
            let data_start = final_pos + 1;
            let data_end = end;
            let sixel_data = &bytes[data_start..data_end];
            
            // Process sixel data
            if let Ok(mut g) = graphics.lock() {
                if let Some(decoder) = g.sixel_decoder_mut() {
                    decoder.process(sixel_data);
                    if let Some(image) = decoder.finalize() {
                        g.add_image(image);
                    }
                } else {
                    let mut decoder = SixelDecoder::new();
                    decoder.process(sixel_data);
                    if let Some(image) = decoder.finalize() {
                        g.add_image(image);
                    }
                    *g.sixel_decoder_mut() = Some(decoder);
                }
            }
            
            // Return consumed bytes and empty remaining (we consume the entire DCS)
            return Some((end + st_len, Vec::new()));
        }
    }
    
    // Not a graphics DCS, pass through
    None
}

/// Handle OSC (Operating System Command) sequences for Kitty graphics
fn handle_osc(bytes: &[u8], graphics: &SharedGraphicsState) -> Option<(usize, Vec<u8>)> {
    // Minimum: ESC ] G ... ESC \ (at least 6 bytes)
    if bytes.len() < 5 || bytes[0] != 0x1B || bytes[1] != 0x5D {
        return None;
    }
    
    // Find the end of OSC (ST = ESC \ = 0x1B 0x5C, or BEL = 0x07, or 0x9C)
    let mut i = 2;
    let mut first_param_end = None;
    
    while i < bytes.len() {
        let b = bytes[i];
        if b == 0x1B && i + 1 < bytes.len() && bytes[i + 1] == 0x5C {
            // Found ESC \ (ST)
            break;
        }
        if b == 0x07 || b == 0x9C {
            // Found BEL or single-byte ST
            break;
        }
        if first_param_end.is_none() && b == 0x3B { // ';'
            first_param_end = Some(i);
        }
        i += 1;
    }
    
    if i >= bytes.len() {
        return None; // Incomplete sequence
    }
    
    let end = i;
    let st_len = if bytes[i] == 0x1B { 2 } else { 1 };
    
    // Check if this is a kitty graphics sequence
    // Kitty uses: ESC ] G <params> ; <data> ST  or  ESC ] 1337 ; File=... ST
    // First param is 'G' (0x47) or '1337'
    let first_param = &bytes[2..first_param_end.unwrap_or(end)];
    
    let is_kitty = first_param == b"G" || first_param == b"1337";
    
    if is_kitty {
        // Extract params and data
        let params_data = &bytes[2..end];
        let mut params = Vec::new();
        let mut current = Vec::new();
        
        for &b in params_data {
            if b == 0x3B { // ';'
                params.push(current);
                current = Vec::new();
            } else {
                current.push(b);
            }
        }
        if !current.is_empty() {
            params.push(current);
        }
        
        // Convert to slice of slices
        let param_slices: Vec<&[u8]> = params.iter().map(|v| v.as_slice()).collect();
        
        // The data is after the last ';' (in the last param for kitty)
        // Kitty format: G=<action>;<param1>;<param2>;...;<data>
        // Or: 1337;File=...;data
        
        let data = if params.len() > 1 {
            params[params.len() - 1].as_slice()
        } else {
            &[]
        };
        
        // Process kitty graphics
        if let Ok(mut g) = graphics.lock() {
            if let Some(decoder) = g.kitty_decoder_mut().as_mut() {
                if let Some(image) = decoder.process(&param_slices, data) {
                    g.add_image(image);
                }
            }
        }
        
        // Return consumed bytes and empty remaining (we consume the entire OSC)
        return Some((end + st_len, Vec::new()));
    }
    
    // Not a graphics OSC, pass through
    None
}