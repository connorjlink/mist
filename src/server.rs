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
use crate::debug_controller::*;
use crate::debug_engine::*;

// Mist server.rs
// (c) Connor J. Link. All Rights Reserved.

const DEFAULT_READ_SIZE: i64 = 0x1000;

#[unsafe(no_mangle)]
pub extern "C" fn mist_initialize(connection_string: *const c_char) {
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
struct DebugServer {
    // breakpoints, variables, etc.
    breakpoints: Vec<String>,
}

type SharedState = Arc<Mutex<DebugServer>>;

async fn start_server(connection_string: &str) {
    let state = Arc::new(Mutex::new(DebugServer::default()));
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
        let message = msg.unwrap();
        if message.is_text() {
            let request: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
            
            let response = match handle_dap_message(&request, &state).await {
                Ok(res) => res,
                Err(err) => {
                    let sequence = request.get("seq").and_then(|s| s.as_i64()).unwrap_or(0);
                    let command = request.get("command").and_then(|c| c.as_str()).unwrap_or("");
                    err.to_dap_error(sequence, command)
                }
            };

            write.send(tokio_tungstenite::tungstenite::Message::Text(response)).await.unwrap();
        }
    }
}

async fn handle_dap_message(request: &Value, state: &SharedState) -> DebuggerResult<String> {
    let command = request.get("command").and_then(|c| c.as_str()).unwrap_or("");
    let sequence = request.get("seq").and_then(|s| s.as_i64()).unwrap_or(0);
    match command {
        "initialize" => {
            let body = InitializeResponseBody {
                supports_configuration_done_request: true,
                supports_function_breakpoints: true,
                supports_modules_request: false,
                supports_read_memory_request: true,
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
            return Ok(dap_success(sequence, "initialize", Some(body)));
        }
        "setFunctionBreakpoints" => {
            let mut names = Vec::new();
            if let Some(bps) = request["arguments"]["breakpoints"].as_array() {
                for bp in bps {
                    if let Some(name) = bp["name"].as_str() {
                        names.push(name.to_string());
                    }
                }
            }

            let verified = set_requested_function_breakpoints(names);
            let breakpoints = verified
                .into_iter()
                .map(|verified| Breakpoint { verified })
                .collect();
            let body = SetFunctionBreakpointsResponseBody { breakpoints };
            return Ok(dap_success(sequence, "setFunctionBreakpoints", Some(body)));
        }
        "setBreakpoints" => {
            let mut state = state.lock().await;
            state.breakpoints.clear();
            let mut response = Vec::new();
            if let Some(breakpoints) = request["arguments"]["breakpoints"].as_array() {
                for breakpoint in breakpoints {
                    if let Some(line) = breakpoint["line"].as_i64() {
                        state.breakpoints.push(format!("line: {}", line));
                        response.push(Breakpoint { verified: true });
                    }
                }
            } 
            // fine to have no breakpoints also to clear existing entries
            let body = SetBreakpointsResponseBody { breakpoints: response };
            return Ok(dap_success(sequence, "setBreakpoints", Some(body)));
        }
        "readMemory" => {
            let address_str = request["arguments"]["offset"].as_str().unwrap_or("");
            let address = parse_address_literal(address_str)
                .ok_or_else(|| DebuggerError(format!("Invalid memory address: {}", address_str)))?;

            let count = request["arguments"]["count"].as_i64().unwrap_or(DEFAULT_READ_SIZE);
            // TODO: finish read memory request
            return Ok(dap_success(sequence, "readMemory", Some(body)));
        }
        "continue" => {
            controller().submit(DebugCommand::Continue);
            return Ok(dap_success(sequence, "continue", None::<()>));
        }
        "stepIn" => {
            controller().submit(DebugCommand::StepIn);
            return Ok(dap_success(sequence, "stepIn", None::<()>));
        }
        "stepOut" => {
            controller().submit(DebugCommand::StepOut);
            return Ok(dap_success(sequence, "stepOut", None::<()>));
        }
        "next" => {
            controller().submit(DebugCommand::StepOver);
            return Ok(dap_success(sequence, "next", None::<()>));
        }
        _ => {
            return Err(DebuggerError("Command not implemented".to_string()));
        }
    }
}
