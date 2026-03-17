use std::thread;
use std::sync::Arc;
use std::ffi::CStr;
use std::os::raw::c_char;
use tokio::runtime::Runtime;
use tokio::sync::Mutex;
use tokio::net::TcpListener;
use tokio_tungstenite::accept_async;
use futures_util::{StreamExt, SinkExt};
use serde_json::Value;

use crate::dap::*;

// Mist server.rs
// (c) Connor J. Link. All Rights Reserved.

#[unsafe(no_mangle)]
pub extern "C" fn initialize(connection_string: *const c_char) {
    // initialize the debugger and start hosting the WebSocket DAP server
    // this is called from C++ compiler .exe
    let address = unsafe { CStr::from_ptr(connection_string) }
        .to_string_lossy()
        .into_owned();

    thread::spawn(move || {
        let runtime = Runtime::new().unwrap();
        runtime.block_on(async {
            start_server(&address).await;
        });
    });
}

#[derive(Default)]
struct DebuggerServer {
    // breakpoints, variables, etc.
    breakpoints: Vec<String>,
}

type SharedState = Arc<Mutex<DebuggerServer>>;

async fn start_server(connection_string: &str) {
    let state = Arc::new(Mutex::new(DebuggerServer::default()));
    let listener = TcpListener::bind(connection_string).await.unwrap();

    while let Ok((stream, _)) = listener.accept().await {
        let state = state.clone();
        tokio::spawn(handle_connection(stream, state));
    }
}

async fn handle_connection(stream: tokio::net::TcpStream, state: SharedState) {
    let ws_stream = accept_async(stream).await.unwrap();
    let (mut write, mut read) = ws_stream.split();

    while let Some(msg) = read.next().await {
        let msg = msg.unwrap();
        if msg.is_text() {
            let req: Value = serde_json::from_str(msg.to_text().unwrap()).unwrap();
            let resp = handle_dap_message(&req, &state).await;
            write.send(tokio_tungstenite::tungstenite::Message::Text(resp)).await.unwrap();
        }
    }
}

async fn handle_dap_message(req: &Value, state: &SharedState) -> String {
    let command = req.get("command").and_then(|c| c.as_str()).unwrap_or("");
    let seq = req.get("seq").and_then(|s| s.as_i64()).unwrap_or(0);
    match command {
        "initialize" => {
            let body = InitializeResponseBody {
                supports_configuration_done_request: true,
                supports_function_breakpoints: false,
                supports_modules_request: false,
                breakpoint_modes: vec![
                    BreakpointMode {
                        mode: "software".to_string(),
                        label: "Software Breakpoint".to_string(),
                        applies_to: vec![
                            "source".to_string(), 
                            "instruction".to_string()
                        ],
                    },
                    BreakpointMode {
                        mode: "hardware".to_string(),
                        label: "Hardware Breakpoint".to_string(),
                        applies_to: vec![
                            "source".to_string(), 
                            "instruction".to_string()
                        ],
                    },
                ],
            };
            return dap_success(seq, "initialize", Some(body));
        }
        "setBreakpoints" => {
            let mut s = state.lock().await;
            s.breakpoints.clear();
            let mut breakpoints = Vec::new();
            if let Some(bps) = req["arguments"]["breakpoints"].as_array() {
                for bp in bps {
                    if let Some(line) = bp["line"].as_i64() {
                        s.breakpoints.push(format!("line: {}", line));
                        breakpoints.push(Breakpoint { verified: true });
                    }
                }
            }
            let body = SetBreakpointsResponseBody { breakpoints };
            return dap_success(seq, "setBreakpoints", Some(body));
        }
        "stepIn" => {
            // TODO: implement stepping logic
            let mut s = state.lock().await;
            return dap_success(seq, "stepIn", None);
        }
        "stepOut" => {
            // TODO: implement stepping logic
            let mut s = state.lock().await;
            return dap_success(seq, "stepOut", None);
        }
        "stepOver" => {
            // TODO: implement stepping logic
            let mut s = state.lock().await;
            return dap_success(seq, "stepOver", None);
        }
        _ => {
            return dap_error(seq, command, "Command not implemented");
        }
    }
}