use std::{os::raw::c_char, thread};

use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::{net::TcpListener, runtime::Runtime};
use tokio_tungstenite::accept_async;

use crate::{dap::*, debug_controller::*, debug_engine::*, utility::*};

// Mist server.rs
// (c) Connor J. Link. All Rights Reserved.

const DEFAULT_READ_SIZE: i64 = 0x1000;

async fn start_server(connection_string: &str)
{
    let listener = TcpListener::bind(connection_string).await.unwrap();

    while let Ok((stream, _)) = listener.accept().await
    {
        tokio::spawn(handle_connection(stream));
    }
}

async fn handle_connection(stream: tokio::net::TcpStream)
{
    let ws_stream = accept_async(stream).await.unwrap();
    let (mut write, mut read) = ws_stream.split();

    while let Some(msg) = read.next().await
    {
        let message = msg.unwrap();
        if message.is_text()
        {
            let request: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();

            let response = match handle_dap_message(&request).await
            {
                Ok(res) => res,
                Err(error) =>
                {
                    let sequence = request.get("seq").and_then(|s| s.as_i64()).unwrap_or(0);
                    let command = request.get("command").and_then(|c| c.as_str()).unwrap_or("");
                    error.to_dap_error(sequence, command)
                }
            };

            write.send(tokio_tungstenite::tungstenite::Message::Text(response)).await.unwrap();
        }
    }
}

async fn handle_dap_message(request: &Value) -> DebuggerResult<String>
{
    let command = request.get("command").and_then(|c| c.as_str()).unwrap_or("");
    let sequence = request.get("seq").and_then(|s| s.as_i64()).unwrap_or(0);
    match command
    {
        "initialize" =>
        {
            let body = InitializeResponseBody {
                supports_configuration_done_request: true,
                supports_function_breakpoints: true,
                supports_modules_request: false,
                supports_read_memory_request: true,
                breakpoint_modes: vec![
                    BreakpointMode {
                        mode: "software".to_string(),
                        label: "Software Breakpoint".to_string(),
                        applies_to: vec!["source".to_string(), "instruction".to_string()]
                    },
                    BreakpointMode {
                        mode: "hardware".to_string(),
                        label: "Hardware Breakpoint".to_string(),
                        applies_to: vec!["source".to_string(), "instruction".to_string()]
                    },
                ]
            };
            return Ok(dap_success(sequence, "initialize", Some(body)));
        },
        "setFunctionBreakpoints" =>
        {
            let mut names = Vec::new();
            if let Some(breakpoints) = request["arguments"]["breakpoints"].as_array()
            {
                for breakpoint in breakpoints
                {
                    if let Some(name) = breakpoint["name"].as_str()
                    {
                        names.push(name.to_string());
                    }
                }
            }

            let verified = set_requested_function_breakpoints(names);
            // TODO: include proper corresponding breakpoint information in the repsonse
            let breakpoints = verified
                .into_iter()
                .map(|verified| Breakpoint {
                    verified: verified,
                    message: None,
                    source: None,
                    line: None,
                    column: None,
                    end_line: None,
                    end_column: None,
                    instruction_reference: None,
                    offset: None
                })
                .collect();

            let body = SetFunctionBreakpointsResponseBody { breakpoints };
            return Ok(dap_success(sequence, "setFunctionBreakpoints", Some(body)));
        },
        "setBreakpoints" =>
        {
            let mut response = Vec::new();
            if let Some(breakpoints) = request["arguments"]["breakpoints"].as_array()
            {
                for breakpoint in breakpoints
                {
                    // TODO: figure out how to fetch line information
                    // TODO: fire over breakpoint information to the debug engine
                    // TODO: populate the proper fields in the response based upon breakpoint information
                    response.push(Breakpoint {
                        verified: false,
                        message: None,
                        source: None,
                        line: None,
                        column: None,
                        end_line: None,
                        end_column: None,
                        instruction_reference: None,
                        offset: None
                    });
                }
            }
            let body = SetBreakpointsResponseBody { breakpoints: response };
            return Ok(dap_success(sequence, "setBreakpoints", Some(body)));
        },
        "readMemory" =>
        {
            let address_str = request["arguments"]["offset"].as_str().unwrap_or("");
            let address = parse_address_literal(address_str)
                .ok_or_else(|| DebuggerError(format!("Invalid memory address: {}", address_str)))?;

            let count = request["arguments"]["count"].as_i64().unwrap_or(DEFAULT_READ_SIZE);
            // TODO: finish read memory request

            let engine = get_engine().lock().unwrap();
            let bytes = read_memory(address, count)
                .map_err(|error| DebuggerError(format!("Failed to read memory: {:?}", error)))?;

            let body = ReadMemoryResponseBody {
                address: format!("0x{:X}", address),
                unreadable_bytes: 0,
                data: base64::engine::general_purpose::STANDARD.encode(bytes)
            };
            return Ok(dap_success(sequence, "readMemory", Some(body)));
        },
        "continue" =>
        {
            controller().submit(DebugCommand::Continue);
            return Ok(dap_success(sequence, "continue", None::<()>));
        },
        "stepIn" =>
        {
            controller().submit(DebugCommand::StepIn);
            return Ok(dap_success(sequence, "stepIn", None::<()>));
        },
        "stepOut" =>
        {
            controller().submit(DebugCommand::StepOut);
            return Ok(dap_success(sequence, "stepOut", None::<()>));
        },
        "next" =>
        {
            controller().submit(DebugCommand::StepOver);
            return Ok(dap_success(sequence, "next", None::<()>));
        },
        _ =>
        {
            return Err(DebuggerError("Command not implemented".to_string()));
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn mist_initialize(connection_string: *const c_char) -> bool
{
    // initialize the debugger and start hosting the WebSocket DAP server
    // this is called from C++ compiler .exe
    let Some(name) = cstr_to_string(connection_string)
    else
    {
        return false;
    };

    thread::spawn(move || {
        let runtime = Runtime::new().unwrap();
        runtime.block_on(async {
            start_server(&name).await;
        });
    });

    return true;
}
