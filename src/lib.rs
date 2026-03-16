use std::thread;
use std::sync::Arc;
use tokio::runtime::Runtime;
use tokio::sync::Mutex;
use tokio::net::TcpListener;
use tokio_tungstenite::accept_async;
use futures_util::{StreamExt, SinkExt};
use serde_json::Value;

mod dap;
use dap::*;

#[unsafe(no_mangle)]
pub extern "C" fn initialize() {
    // Initialize the debugger and start hosting the WebSocket DAP server.
    thread::spawn(|| {
        let rt = Runtime::new().unwrap();
        rt.block_on(async {
            start_server().await;
        });
    });
}

#[derive(Default)]
struct DebuggerState {
    // breakpoints, variables, etc.
    breakpoints: Vec<String>,
}

type SharedState = Arc<Mutex<DebuggerState>>;

async fn start_server() {
    let state = Arc::new(Mutex::new(DebuggerState::default()));
    let listener = TcpListener::bind("127.0.0.1:9000").await.unwrap();

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
        _ => {
            return dap_error(seq, command, "Command not implemented");
        }
    }
}