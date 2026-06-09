use std::sync::{Condvar, Mutex, OnceLock};

// Mist debug_controller.rs
// (c) Connor J. Link. All Rights Reserved.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DebugCommand
{
    Continue,
    StepIn,
    StepOver,
    StepOut
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason
{
    Breakpoint,
    SingleStep,
    ProcessExit,
    Unknown
}

#[derive(Debug, Default)]
struct ControllerState
{
    pending_command: Option<DebugCommand>,
    last_stop_reason: Option<StopReason>,
    last_stop_thread_id: Option<u32>,
    is_active: bool
}

pub struct DebugController
{
    state: Mutex<ControllerState>,
    condition_variable: Condvar
}

static CONTROLLER: OnceLock<DebugController> = OnceLock::new();
pub fn controller() -> &'static DebugController
{
    CONTROLLER.get_or_init(DebugController::new)
}

impl DebugController
{
    pub fn new() -> Self
    {
        return Self { state: Mutex::new(ControllerState::default()), condition_variable: Condvar::new() };
    }

    pub fn set_session_active(&self, active: bool)
    {
        let mut state = self.state.lock().unwrap();
        state.is_active = active;
        return self.condition_variable.notify_all();
    }

    pub fn is_session_active(&self) -> bool
    {
        let state = self.state.lock().unwrap();
        return state.is_active;
    }

    pub fn submit(&self, command: DebugCommand)
    {
        let mut state = self.state.lock().unwrap();
        state.pending_command = Some(command);
        return self.condition_variable.notify_all();
    }

    pub fn notify_stop(&self, reason: StopReason, thread_id: u32)
    {
        let mut state = self.state.lock().unwrap();
        state.last_stop_reason = Some(reason);
        state.last_stop_thread_id = Some(thread_id);
        return self.condition_variable.notify_all();
    }

    pub fn wait_for_command(&self) -> DebugCommand
    {
        let mut state = self.state.lock().unwrap();
        loop
        {
            if let Some(command) = state.pending_command.take()
            {
                return command;
            }
            state = self.condition_variable.wait(state).unwrap();
        }
    }

    pub fn try_take_command(&self) -> Option<DebugCommand>
    {
        let mut state = self.state.lock().unwrap();
        return state.pending_command.take();
    }
}
