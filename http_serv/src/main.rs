use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;

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

    let response = if path == "/" {
        handle_root_json_response()
    } else if path.starts_with("/proc/") && path.ends_with("/status") {
        handle_proc_status_json_response(path)
    } else {
        handle_proc_file_response(path)
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
                        processes.push(format!(
                            r#"{{"pid": {}, "name": "{}", "cpu": {:.2}, "mem_kb": {}}}"#,
                            pid, parts[1], cpu, rss
                        ));
                    }
                }
            }

            let json_array = format!("[{}]", processes.join(","));
            let body = format!(r#"{{"processes": {}}}"#, json_array);

            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            )
        }
        Err(_) => "HTTP/1.1 500 Internal Server Error\r\n\r\nFailed to run 'ps'.".to_string(),
    }
}

fn handle_proc_status_json_response(path: &str) -> String {
    let parts: Vec<&str> = path.trim_start_matches("/proc/").split('/').collect();
    if parts.len() < 2 {
        return "HTTP/1.1 400 Bad Request\r\n\r\nInvalid /proc status path.".to_string();
    }
    let pid = parts[0];

    let file_path = PathBuf::from("/proc").join(pid).join("status");
    match std::fs::read_to_string(&file_path) {
        Ok(contents) => {
            let json_map = parse_status_to_json(&contents);
            let body = serde_json::to_string_pretty(&json_map).unwrap_or_else(|_| "{}".to_string());

            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            )
        }
        Err(_) => "HTTP/1.1 404 Not Found\r\n\r\nFile not found.".to_string(),
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

fn handle_proc_file_response(path: &str) -> String {
    let clean_path = Path::new(path)
        .components()
        .filter_map(|comp| match comp {
            std::path::Component::Normal(part) => Some(part),
            _ => None,
        })
        .collect::<PathBuf>();

    let file_path = PathBuf::from("/proc").join(clean_path);

    match std::fs::read_to_string(&file_path) {
        Ok(contents) => format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n{}",
            contents.len(),
            contents
        ),
        Err(_) => "HTTP/1.1 404 Not Found\r\n\r\nFile not found.".to_string(),
    }
}

