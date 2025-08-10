use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::Command;
use std::thread;

use serde_json::json;

const ACCESS_KEY: &str = "debugger";

fn main() {
    let listener = TcpListener::bind("0.0.0.0:7878").unwrap();
    println!("Server running at http://0.0.0.0:7878");

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                thread::spawn(move || {
                    handle_connection(stream);
                });
            }
            Err(e) => eprintln!("Connection failed: {}", e),
        }
    }
}

fn handle_connection(mut stream: TcpStream) {
    let mut reader = BufReader::new(&stream);
    let mut request_line = String::new();

    if reader.read_line(&mut request_line).is_err() {
        eprintln!("Failed to read request line");
        return;
    }

    let parts: Vec<&str> = request_line.trim().split_whitespace().collect();
    if parts.len() < 2 {
        let _ = stream.write_all(b"HTTP/1.1 400 Bad Request\r\n\r\nInvalid request line");
        return;
    }

    let method = parts[0];
    let full_path = parts[1];

    if method != "GET" {
        let _ = stream.write_all(b"HTTP/1.1 405 Method Not Allowed\r\n\r\nOnly GET is supported.");
        return;
    }

    let (path, query) = match full_path.find('?') {
        Some(idx) => (&full_path[..idx], &full_path[idx + 1..]),
        None => (full_path, ""),
    };

    let authorized = query
        .split('&')
        .any(|param| param == format!("key={}", ACCESS_KEY));

    if !authorized {
        let _ = stream.write_all(b"HTTP/1.1 403 Forbidden\r\n\r\nAccess denied.");
        return;
    }

    println!("Request: {}", full_path);

    let response = match path {
        "/" => handle_root_json_response(),
        "/proc" => handle_proc_list_response(),
        _ if path.starts_with("/proc/") => handle_proc_path_response(path),
        _ => json_error_response(404, "Not Found"),
    };

    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

fn handle_root_json_response() -> String {
    match Command::new("ps")
        .args(&["-eo", "pid,comm,%cpu,rss"])
        .output()
    {
        Ok(output) => {
            let contents = String::from_utf8_lossy(&output.stdout);
            let mut processes = Vec::new();

            for line in contents.lines().skip(1) {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 4 {
                    if let (Ok(pid), Ok(cpu), Ok(rss)) = (
                        parts[0].parse::<u32>(),
                        parts[2].parse::<f32>(),
                        parts[3].parse::<u64>(),
                    ) {
                        processes.push(json!({
                            "pid": pid,
                            "name": parts[1],
                            "cpu": cpu,
                            "mem_kb": rss
                        }));
                    }
                }
            }

            let body = json!({ "processes": processes }).to_string();

            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            )
        }
        Err(_) => json_error_response(500, "Failed to run 'ps'"),
    }
}

// New handler: list all numeric directories in /proc as PIDs
fn handle_proc_list_response() -> String {
    match fs::read_dir("/proc") {
        Ok(entries) => {
            let mut pids = Vec::new();
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.chars().all(|c| c.is_ascii_digit()) {
                    pids.push(name);
                }
            }

            let body = json!({ "pids": pids }).to_string();

            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            )
        }
        Err(_) => json_error_response(500, "Failed to read /proc directory"),
    }
}

// New handler: list files inside /proc/{pid}
fn handle_proc_path_response(path: &str) -> String {
    let trimmed = path.trim_start_matches("/proc/");
    let mut parts = trimmed.split('/');

    let pid = match parts.next() {
        Some(p) if p.chars().all(|c| c.is_ascii_digit()) => p,
        _ => return json_error_response(400, "Invalid PID"),
    };

    match parts.next() {
        None => {
            // No file specified → list files for that PID
            let proc_pid_path = PathBuf::from("/proc").join(pid);
            match fs::read_dir(proc_pid_path) {
                Ok(entries) => {
                    let mut files = Vec::new();
                    for entry in entries.flatten() {
                        let fname = entry.file_name().to_string_lossy().to_string();
                        files.push(fname);
                    }
                    let body = json!({ "files": files }).to_string();

                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                        body.len(),
                        body
                    )
                }
                Err(_) => json_error_response(404, "PID directory not found"),
            }
        }
        Some(filename) => {
            // File requested — serve file contents (text/plain or JSON if status)
            let file_path = PathBuf::from("/proc").join(pid).join(filename);

            match fs::read_to_string(&file_path) {
                Ok(contents) => {
                    // If "status" file, parse to JSON
                    if filename == "status" {
                        let map = parse_status_to_json(&contents);
                        let body = serde_json::to_string_pretty(&map).unwrap_or_else(|_| "{}".to_string());
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                            body.len(),
                            body
                        )
                    } else {
                        // Serve plain text for other files
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n{}",
                            contents.len(),
                            contents
                        )
                    }
                }
                Err(_) => json_error_response(404, "File not found or unreadable"),
            }
        }
    }
}

fn parse_status_to_json(status_text: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for line in status_text.lines() {
        if let Some(idx) = line.find(':') {
            let key = line[..idx].trim().to_string();
            let value = line[idx + 1..].trim().to_string();
            map.insert(key, value);
        }
    }
    map
}

fn json_error_response(code: u16, message: &str) -> String {
    let body = json!({ "error": message }).to_string();
    format!(
        "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        code,
        http_status_text(code),
        body.len(),
        body
    )
}

fn http_status_text(code: u16) -> &'static str {
    match code {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        500 => "Internal Server Error",
        _ => "Unknown",
    }
}

