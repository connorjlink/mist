use serde::{Serialize, Deserialize};

#[allow(non_snake_case)]

#[derive(Serialize)]
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

#[derive(Serialize)]
pub struct BreakpointMode {
    pub mode: String,
    pub label: String,
    pub appliesTo: Vec<String>,
}

#[derive(Serialize)]
pub struct InitializeResponseBody {
    pub supportsConfigurationDoneRequest: bool,
    pub supportsFunctionBreakpoints: bool,
    pub supportsModulesRequest: bool,
    pub breakpointModes: Vec<BreakpointMode>,
}

#[derive(Serialize)]
pub struct Breakpoint {
    pub verified: bool,
}

#[derive(Serialize)]
pub struct SetBreakpointsResponseBody {
    pub breakpoints: Vec<Breakpoint>,
}
