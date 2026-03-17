use std::collections::HashMap;
use std::rc::Rc;
use std::cell::RefCell;

use windows::{
    Win32::{
        Foundation::*,
        System::{
            Diagnostics::Debug::*,
            Threading::*,
        },
    },
};

mod utilities;
mod tooling;
mod debugger;

use utilities::*;

// Mist main.rs
// (c) Connor J. Link. All Rights Reserved.

pub struct ArgumentParser {
    handlers: HashMap<String, Box<dyn FnMut(&str)>>,
}

impl ArgumentParser {
    pub fn new() -> Self {
        return Self { 
            handlers: HashMap::new() 
        };
    }

    pub fn on<F>(&mut self, name: &str, callback: F)
        where F: FnMut(&str) + 'static, {
        self.handlers.insert(name.to_string(), Box::new(callback));
    }

    pub fn parse(&mut self, args: &[String]) {
        for arg in args {
            if let Some(stripped) = arg.strip_prefix("--") {
                if let Some((name, value)) = stripped.split_once('=') {
                    if let Some(handler) = self.handlers.get_mut(name) {
                        handler(value);
                    }
                }
            }
        }
    }
}

fn main() {

    let args = std::env::args().collect::<Vec<String>>();

    let mut parser = ArgumentParser::new();

    let target = Rc::new(RefCell::new(None));
    parser.on("target", {
        let target = Rc::clone(&target);
        move |value| {
            *target.borrow_mut() = Some(value.to_string());
            println!("Target: {}", value);
        }
    });

    parser.parse(&args);

    // argument syntax `mist.exe --target=<executable_path>`
    let target = args.iter()
        .find(|arg| arg.starts_with("--target="))
        .and_then(|arg| {
            let mut parts = arg.splitn(2, '=');
            parts.next();
            parts.next().map(|s| s.to_string())
        });

    if target.is_none() {
        eprintln!("Usage: mist.exe --target=<executable_path>");
        return;
    }

    let app_path = to_pcstr(&target.unwrap());

    let mut startup_info = STARTUPINFOA::default();
    startup_info.cb = std::mem::size_of::<STARTUPINFOA>() as u32;

    let mut process_info = PROCESS_INFORMATION::default();

    unsafe {
        let success = CreateProcessA(
            app_path,
            None,
            None,
            None,
            false,
            DEBUG_ONLY_THIS_PROCESS,
            None,
            None,
            &mut startup_info,
            &mut process_info,
        );

        if success.is_err() {
            eprintln!("Failed to create process: {:?}", success.err());
            return;
        }

        let mut debug_event = DEBUG_EVENT::default();

        // Primary debugging loop
        loop {
            if WaitForDebugEvent(&mut debug_event, INFINITE).is_ok() {
                match debug_event.dwDebugEventCode {
                    CREATE_PROCESS_DEBUG_EVENT => {
                        println!("Process created: Entry point (image base): {:?}", 
                            debug_event.u.CreateProcessInfo.lpBaseOfImage);

                        
                        // You can read memory or set breakpoints here
                    },
                    EXIT_PROCESS_DEBUG_EVENT => {
                        println!("Process exited");
                        break;
                    },
                    _ => {}
                }

                let _ = ContinueDebugEvent(
                    debug_event.dwProcessId,
                    debug_event.dwThreadId,
                    DBG_CONTINUE,
                );
            }
        }
    }
}
