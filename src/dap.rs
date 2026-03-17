use serde::{Serialize, Deserialize};

// Mist dap.rs
// (c) Connor J. Link. All Rights Reserved.

#[derive(Serialize, Deserialize)]
pub struct DapResponse<T> {
    #[serde(rename = "type")]
    msg_type: &'static str,
    request_seq: i64,
    success: bool,
    command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    body: Option<T>,
}

pub fn dap_success<T: Serialize>(seq: i64, command: &str, body: Option<T>) -> String {
    let resp = DapResponse {
        msg_type: "response",
        request_seq: seq,
        success: true,
        command: command.to_string(),
        message: None,
        body,
    };
    return serde_json::to_string(&resp).unwrap();
}

pub fn dap_error(seq: i64, command: &str, message: &str) -> String {
    let resp: DapResponse<()> = DapResponse {
        msg_type: "response",
        request_seq: seq,
        success: false,
        command: command.to_string(),
        message: Some(message.to_string()),
        body: None,
    };
    return serde_json::to_string(&resp).unwrap();
}

#[derive(Serialize, Deserialize)]
pub struct BreakpointMode {
    pub mode: String,
    pub label: String,
    #[serde(rename = "appliesTo")]
    pub applies_to: Vec<String>,
}

#[derive(Serialize, Deserialize)]
pub struct InitializeResponseBody {
    #[serde(rename = "supportsConfigurationDoneRequest")]
    pub supports_configuration_done_request: bool,
    #[serde(rename = "supportsFunctionBreakpoints")]
    pub supports_function_breakpoints: bool,
    #[serde(rename = "supportsModulesRequest")]
    pub supports_modules_request: bool,
    #[serde(rename = "breakpointModes")]
    pub breakpoint_modes: Vec<BreakpointMode>,
}

#[derive(Serialize, Deserialize)]
pub struct Breakpoint {
    pub verified: bool,
}

#[derive(Serialize, Deserialize)]
pub struct SetBreakpointsResponseBody {
    pub breakpoints: Vec<Breakpoint>,
}

#[derive(Serialize, Deserialize)]
pub struct SetFunctionBreakpointsResponseBody {
    pub breakpoints: Vec<Breakpoint>,
}
