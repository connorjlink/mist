use serde::{Deserialize, Serialize};

// Mist dap.rs
// (c) Connor J. Link. All Rights Reserved.

#[derive(Serialize, Deserialize)]
pub struct DapResponse<T>
{
    #[serde(rename = "type")]
    msg_type: &'static str,
    request_seq: i64,
    success: bool,
    command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    body: Option<T>
}

pub fn dap_success<T: Serialize>(seq: i64, command: &str, body: Option<T>) -> String
{
    let response = DapResponse {
        msg_type: "response",
        request_seq: seq,
        success: true,
        command: command.to_string(),
        message: None,
        body
    };
    return serde_json::to_string(&response).unwrap();
}

pub fn dap_error(seq: i64, command: &str, message: &str) -> String
{
    let response: DapResponse<()> = DapResponse {
        msg_type: "response",
        request_seq: seq,
        success: false,
        command: command.to_string(),
        message: Some(message.to_string()),
        body: None
    };
    return serde_json::to_string(&response).unwrap();
}

#[derive(Serialize, Deserialize)]
pub struct BreakpointMode
{
    pub mode: String,
    pub label: String,
    #[serde(rename = "appliesTo")]
    pub applies_to: Vec<String>
}

#[derive(Serialize, Deserialize)]
pub struct InitializeResponseBody
{
    #[serde(rename = "supportsConfigurationDoneRequest")]
    pub supports_configuration_done_request: bool,
    #[serde(rename = "supportsFunctionBreakpoints")]
    pub supports_function_breakpoints: bool,
    #[serde(rename = "supportsModulesRequest")]
    pub supports_modules_request: bool,
    #[serde(rename = "supportsReadMemoryRequest")]
    pub supports_read_memory_request: bool,
    #[serde(rename = "breakpointModes")]
    pub breakpoint_modes: Vec<BreakpointMode>
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Hash)]
pub enum ChecksumAlgorithm
{
    MD5,
    SHA1,
    SHA256
}

#[derive(Serialize, Deserialize)]
pub struct Checksum
{
    pub algorithm: ChecksumAlgorithm,
    pub checksum: String
}

#[derive(Serialize, Deserialize)]
pub struct Source
{
    pub name: Option<String>,
    pub path: Option<String>,
    // omitting sourceReference and presentationHint for
    pub origin: Option<String>,
    pub sources: Option<Vec<Source>>,
    pub checksums: Option<Vec<Checksum>>
}

#[derive(Serialize, Deserialize)]
pub struct Breakpoint
{
    pub verified: bool,
    pub message: Option<String>,
    pub source: Option<Source>,
    pub line: Option<u32>,
    pub column: Option<u32>,
    pub end_line: Option<u32>,
    pub end_column: Option<u32>,
    pub instruction_reference: Option<String>, // memory address of actual breakpoint
    pub offset: Option<u64>
}

#[derive(Serialize, Deserialize)]
pub struct Variable
{
    pub name: String,
    pub value: String,
    #[serde(rename = "type")]
    pub r#type: Option<String>
}

#[derive(Serialize, Deserialize)]
pub struct SetBreakpointsResponseBody
{
    pub breakpoints: Vec<Breakpoint>
}

#[derive(Serialize, Deserialize)]
pub struct ReadMemoryResponseBody
{
    pub address: String,
    #[serde(rename = "unreadableBytes")]
    pub unreadable_bytes: i64,
    pub data: String
}

#[derive(Serialize, Deserialize)]
pub struct SetFunctionBreakpointsResponseBody
{
    pub breakpoints: Vec<Breakpoint>
}

#[derive(Debug)]
pub struct DebuggerError(pub String);

impl DebuggerError
{
    pub fn to_dap_error(&self, seq: i64, command: &str) -> String
    {
        return dap_error(seq, command, &self.0);
    }
}

pub type DebuggerResult<T> = Result<T, DebuggerError>;
