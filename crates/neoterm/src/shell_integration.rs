//! Shell integration for OSC 7 (cwd), OSC 133 (prompt mark), OSC 633 (command status)

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Shell integration state for a pane
#[derive(Debug, Clone, Default)]
pub struct ShellIntegration {
    /// Current working directory from OSC 7
    pub cwd: Option<PathBuf>,
    /// Prompt markers from OSC 133 (line numbers where prompts appeared)
    pub prompt_markers: Vec<usize>,
    /// Last command exit status from OSC 633
    pub last_command_status: Option<i32>,
}

/// Thread-safe shell integration state
pub type SharedShellIntegration = Arc<Mutex<ShellIntegration>>;

/// Create a new shared shell integration state
pub fn new_shared_shell_integration() -> SharedShellIntegration {
    Arc::new(Mutex::new(ShellIntegration::default()))
}

/// Parse a file:// URL from OSC 7 into a PathBuf
fn parse_osc7_url(url: &str) -> Option<PathBuf> {
    // OSC 7 format: file://HOSTNAME/PATH or file:///PATH
    // We only care about the path component
    let url = url.strip_prefix("file://")?;
    
    // Handle file://HOSTNAME/PATH - skip hostname
    let path_start = if let Some(idx) = url.find('/') {
        idx
    } else {
        0
    };
    
    let path = &url[path_start..];
    // Percent-decode the path
    percent_encoding::percent_decode_str(path)
        .decode_utf8()
        .ok()
        .map(|s| PathBuf::from(s.as_ref()))
}

/// Pre-process PTY output to handle shell integration escape sequences (OSC 7, 133, 633).
///
/// This scans the input bytes for shell integration protocols and
/// delegates to the shell integration state. Returns the remaining bytes that
/// should be passed to the regular vte processor.
pub fn preprocess_shell_integration(
    bytes: &[u8],
    shell_integration: &SharedShellIntegration,
) -> Vec<u8> {
    let mut output = Vec::with_capacity(bytes.len());
    let mut i = 0;
    
    while i < bytes.len() {
        // Check for ESC (0x1B) - start of escape sequences
        if bytes[i] == 0x1B && i + 1 < bytes.len() {
            // OSC (Operating System Command) - 0x5D ']'
            if bytes[i + 1] == 0x5D {
                if let Some((consumed, remaining)) = handle_shell_osc(&bytes[i..], shell_integration) {
                    i += consumed;
                    output.extend_from_slice(&remaining);
                    continue;
                }
            }
        }
        output.push(bytes[i]);
        i += 1;
    }
    
    output
}

/// Handle OSC sequences for shell integration
/// Returns (consumed_bytes, remaining_bytes) or None if not a shell integration OSC
fn handle_shell_osc(
    bytes: &[u8],
    shell_integration: &SharedShellIntegration,
) -> Option<(usize, Vec<u8>)> {
    // OSC format: ESC ] Ps ; Pt ST
    // ST = ESC \ (0x1B 0x5C) or BEL (0x07) or 0x9C
    if bytes.len() < 4 || bytes[0] != 0x1B || bytes[1] != 0x5D {
        return None;
    }
    
    // Find the end of OSC (ST or BEL)
    let mut end_idx = None;
    let mut end_exclusive = None; // end position exclusive (for slicing)
    for i in 2..bytes.len() {
        if bytes[i] == 0x07 || bytes[i] == 0x9C {
            end_idx = Some(i);
            end_exclusive = Some(i);
            break;
        }
        if bytes[i] == 0x1B && i + 1 < bytes.len() && bytes[i + 1] == 0x5C {
            // ESC \ - end at i (exclusive), don't include the backslash
            end_idx = Some(i + 1);
            end_exclusive = Some(i);
            break;
        }
    }
    
    let end = end_idx?;
    let end_excl = end_exclusive.unwrap_or(end);
    let osc_content = &bytes[2..end_excl];
    
    // Parse OSC content: Ps ; Pt
    let content_str = std::str::from_utf8(osc_content).ok()?;
    let parts: Vec<&str> = content_str.split(';').collect();
    if parts.len() < 2 {
        return None;
    }
    
    let ps = parts[0];  // OSC number (7, 133, 633)
    let pt = parts[1..].join(";");  // Parameter text (may contain semicolons)
    
    let mut si = shell_integration.lock().ok()?;
    
    let handled = match ps {
        "7" => {
            // OSC 7: Current working directory
            // Format: ESC ] 7 ; file://HOSTNAME/PATH ESC \
            if let Some(path) = parse_osc7_url(&pt) {
                si.cwd = Some(path);
            }
            true
        }
        "133" => {
            // OSC 133: Prompt mark
            // Format: ESC ] 133 ; A ESC \  (A = "A" for prompt start)
            // We track the line number where the prompt appears
            // Note: The actual line tracking would need the current cursor row from the terminal
            // For now, we just mark that a prompt occurred
            if pt == "A" {
                // Prompt started - we'd need the current cursor row from the terminal
                // This is a simplified version
                si.prompt_markers.push(0); // Placeholder - actual row would come from term
            }
            true
        }
        "633" => {
            // OSC 633: Command status
            // Format: ESC ] 633 ; C ; STATUS ESC \  (C = "C" for command finished)
            // STATUS is the exit code
            let status_parts: Vec<&str> = pt.split(';').collect();
            if status_parts.len() >= 2 && status_parts[0] == "C" {
                if let Ok(status) = status_parts[1].parse::<i32>() {
                    si.last_command_status = Some(status);
                }
            }
            true
        }
        _ => false,
    };
    
    // Only consume OSC sequences we handle; let others pass through to vte
    if handled {
        Some((end + 1, Vec::new()))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_parse_osc7_url() {
        assert_eq!(
            parse_osc7_url("file:///home/user"),
            Some(PathBuf::from("/home/user"))
        );
        assert_eq!(
            parse_osc7_url("file://hostname/home/user"),
            Some(PathBuf::from("/home/user"))
        );
        assert_eq!(
            parse_osc7_url("file:///home/user%20name"),
            Some(PathBuf::from("/home/user name"))
        );
        assert_eq!(parse_osc7_url("http://example.com"), None);
    }
    
    #[test]
    fn test_preprocess_shell_integration() {
        let si = new_shared_shell_integration();
        
        // Test OSC 7
        let osc7 = b"\x1b]7;file:///home/user\x1b\\";
        let result = preprocess_shell_integration(osc7, &si);
        assert!(result.is_empty()); // OSC consumed
        assert_eq!(si.lock().unwrap().cwd, Some(PathBuf::from("/home/user")));
        
        // Test OSC 133
        let osc133 = b"\x1b]133;A\x1b\\";
        let result = preprocess_shell_integration(osc133, &si);
        assert!(result.is_empty());
        assert_eq!(si.lock().unwrap().prompt_markers.len(), 1);
        
        // Test OSC 633
        let osc633 = b"\x1b]633;C;0\x1b\\";
        let result = preprocess_shell_integration(osc633, &si);
        assert!(result.is_empty());
        assert_eq!(si.lock().unwrap().last_command_status, Some(0));
        
        // Test non-shell OSC passes through
        let osc8 = b"\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\";
        let result = preprocess_shell_integration(osc8, &si);
        assert_eq!(result, osc8); // Passes through unchanged
    }
}