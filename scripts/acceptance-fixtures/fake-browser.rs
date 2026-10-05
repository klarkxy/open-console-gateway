//! Capturing Edge/Chrome stand-in for general CLI acceptance.
//! Not a shipping browser. Writes argv/URL/profile and waits for the owner to stop it.

#![windows_subsystem = "windows"]

use std::env;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

fn json_string(value: &str) -> String {
    let mut out = String::from('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

fn write_capture(path: &Path, body: &[u8]) {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(mut file) = File::create(path) {
        let _ = file.write_all(body);
        let _ = file.flush();
    }
}

fn main() {
    let argv: Vec<String> = env::args().skip(1).collect();
    let mut user_data_dir = String::new();
    let mut url = String::new();
    for arg in &argv {
        if let Some(rest) = arg.strip_prefix("--user-data-dir=") {
            user_data_dir = rest.to_string();
        } else if arg.starts_with("https://") || arg.starts_with("http://") {
            url = arg.clone();
        }
    }
    let mut argv_json = String::from('[');
    for (index, arg) in argv.iter().enumerate() {
        if index > 0 {
            argv_json.push(',');
        }
        argv_json.push_str(&json_string(arg));
    }
    argv_json.push(']');
    let body = format!(
        "{{\"pid\":{},\"url\":{},\"userDataDir\":{},\"argv\":{}}}\n",
        std::process::id(),
        json_string(&url),
        json_string(&user_data_dir),
        argv_json
    );
    let bytes = body.into_bytes();
    if let Ok(exe) = env::current_exe() {
        if let Some(dir) = exe.parent() {
            write_capture(&dir.join("ocg-browser-capture.json"), &bytes);
        }
    }
    if !user_data_dir.is_empty() {
        write_capture(
            &PathBuf::from(&user_data_dir).join("ocg-browser-capture.json"),
            &bytes,
        );
    }
    loop {
        thread::sleep(Duration::from_secs(60));
    }
}
