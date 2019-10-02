//! runaway-cli/exit.rs
//! Author: Alexandre Péré
//!
//! This module contains structure for exit codes messages and values.


//--------------------------------------------------------------------------------------------- EXIT


pub enum Exit {
    LoadHostConfiguration,
    SpawnHost,
    ScriptPath,
    ScriptFolder,
    SendIgnoreNotFound,
    SendIgnoreRead,
    ReadFolder,
}

impl std::fmt::Display for Exit {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Exit::LoadHostConfiguration => write!(f, "can not load host configuration"),
            Exit::SpawnHost => write!(f, "can not spawn host"),
            Exit::ScriptPath => write!(f, "can not get absolute script path"),
            Exit::ScriptFolder => write!(f, "can not get script folder path"),
            Exit::SendIgnoreNotFound => write!(f, "send ignore file was provided but not found"),
            Exit::SendIgnoreRead => write!(f, "send ignore file could not be read"),
            Exit::ReadFolder => write!(f, "failed to read local folder"),
        }
    }
}

impl From<Exit> for i32 {
    fn from(exit: Exit) -> i32 {
        match exit {
            Exit::LoadHostConfiguration => 1,
            Exit::SpawnHost => 2,
            Exit::ScriptPath => 3,
            Exit::ScriptFolder => 4,
            Exit::SendIgnoreNotFound => 5,
            Exit::SendIgnoreRead => 6,
            Exit::ReadFolder => 7,
        }
    }
}
