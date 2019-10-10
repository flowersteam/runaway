//! runaway-cli/exit.rs
//! Author: Alexandre Péré
//!
//! This module contains structure for exit codes messages and values.


//--------------------------------------------------------------------------------------------- EXIT


pub enum Exit {
    AllGood,
    LoadHostConfiguration,
    SpawnHost,
    ScriptPath,
    ScriptFolder,
    SendIgnoreNotFound,
    FetchIgnoreNotFound,
    SendIgnoreRead,
    FetchIgnoreRead,
    ReadLocalFolder,
    ReadRemoteFolder,
    PackLocalArchive,
    PackRemoteArchive,
    UnpackLocalArchive,
    UnpackRemoteArchive,
    NodeAcquisition,
    SendArchive,
    FetchArchive,
    ComputeRemoteHash,
    ComputeLocalHash,
    Send,
    Fetch,
    Execute,
    CheckPresence,
    RemoveArchive,
    OutputFolder,
    Cleanup,
    CheckRemotePresence,
    CreateRemoteFolder,
    WrongRemoteFolderString,
    ScriptFailedWithCode(i32),
    ScriptFailedWithoutCode,
    LoadArgumentsFile,
    LoadRemotesFile,
    LoadOutputsFile,
    SpawnThreadPool,
    RemotesExhausted,
    OutputsExhausted,
    ArgumentsExhausted,
    PostProcFailed,
    ExecutionSpawnFailed,
    SomeExecutionFailed(u32),
}

impl std::fmt::Display for Exit {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Exit::AllGood => write!(f, "everything went fine"),
            Exit::LoadHostConfiguration => write!(f, "can not load host configuration"),
            Exit::SpawnHost => write!(f, "can not spawn host"),
            Exit::ScriptPath => write!(f, "script path does not point to an existing file"),
            Exit::ScriptFolder => write!(f, "can not get script folder path"),
            Exit::SendIgnoreNotFound => write!(f, "send ignore file was provided but not found"),
            Exit::FetchIgnoreNotFound => write!(f, "fetch ignore file was provided but not found"),
            Exit::SendIgnoreRead => write!(f, "send ignore file could not be read"),
            Exit::FetchIgnoreRead => write!(f, "fetch ignore file could not be read"),
            Exit::ReadLocalFolder => write!(f, "failed to read local folder"),
            Exit::ReadRemoteFolder => write!(f, "failed to read remote folder"),
            Exit::PackLocalArchive => write!(f, "failed to pack local archive"),
            Exit::PackRemoteArchive => write!(f, "failed to pack remote archive"),
            Exit::UnpackLocalArchive => write!(f, "failed to unpack local archive"),
            Exit::UnpackRemoteArchive => write!(f, "failed to unpack remote archive"),
            Exit::NodeAcquisition => write!(f, "failed to acquire node"),
            Exit::SendArchive => write!(f, "failed to send archive"),
            Exit::FetchArchive => write!(f, "failed to fetch archive"),
            Exit::ComputeRemoteHash => write!(f, "failed to compute hash on remote end"),
            Exit::ComputeLocalHash => write!(f, "failed to compute hash on local end"),
            Exit::Send => write!(f, "failed to send data to the remote end"),
            Exit::Fetch => write!(f, "failed to fetch data from the remote end"),
            Exit::Execute => write!(f, "failed to execute the program"),
            Exit::CheckPresence => write!(f, "failed to check remote archive presence"),
            Exit::RemoveArchive => write!(f, "failed to remove archive"),
            Exit::OutputFolder => write!(f, "failed to create output archive"),
            Exit::Cleanup => write!(f, "failed to clean executions"),
            Exit::CheckRemotePresence => write!(f, "failed to check presence of remote folder"),
            Exit::CreateRemoteFolder => write!(f, "failed to create remote folder"),
            Exit::WrongRemoteFolderString => write!(f, "remote folder template string is not absolute"),
            Exit::ScriptFailedWithCode(ecode) => write!(f, "script failed with error code {}", ecode),
            Exit::ScriptFailedWithoutCode => write!(f, "script failed without exit code"),
            Exit::LoadArgumentsFile => write!(f, "the provided arguments file could not be loaded"),
            Exit::LoadRemotesFile => write!(f, "the provided remotes file could not be loaded"),
            Exit::LoadOutputsFile => write!(f, "the provided outputs file could not be loaded"),
            Exit::SpawnThreadPool => write!(f, "could not spawn the executor thread pool"),
            Exit::RemotesExhausted => write!(f, "wrong remote folders template: remote folders \
                                                 exhausted before arguments"),
            Exit::OutputsExhausted => write!(f, "wrong output folders template: output folders \
                                                 exhausted before arguments"),
            Exit::ArgumentsExhausted => write!(f, "wrong arguments template: arguments exhausted \
                                                   after both remote and output folders"),
            Exit::PostProcFailed => write!(f, "failed to execute the post-processing"),
            Exit::ExecutionSpawnFailed => write!(f, "failed to spawn the execution"),
            Exit::SomeExecutionFailed(nb) => write!(f, "{} executions failed", nb)

            }
    }
}

impl From<Exit> for i32 {
    fn from(exit: Exit) -> i32 {
        match exit {
            Exit::AllGood => 0,
            Exit::LoadHostConfiguration => 991,
            Exit::SpawnHost => 992,
            Exit::ScriptPath => 993,
            Exit::ScriptFolder => 994,
            Exit::SendIgnoreNotFound => 995,
            Exit::FetchIgnoreNotFound => 996,
            Exit::SendIgnoreRead => 997,
            Exit::FetchIgnoreRead => 998,
            Exit::ReadLocalFolder => 999,
            Exit::ReadRemoteFolder => 9910,
            Exit::PackLocalArchive => 9911,
            Exit::PackRemoteArchive => 9912,
            Exit::UnpackLocalArchive => 9913,
            Exit::UnpackRemoteArchive => 9914,
            Exit::NodeAcquisition => 9915,
            Exit::SendArchive => 9916,
            Exit::FetchArchive => 9917,
            Exit::ComputeRemoteHash => 9918,
            Exit::ComputeLocalHash => 9919,
            Exit::Send => 9920,
            Exit::Fetch => 9921,
            Exit::Execute => 9922,
            Exit::CheckPresence => 9923,
            Exit::RemoveArchive => 9924,
            Exit::OutputFolder => 9925,
            Exit::Cleanup => 9926,
            Exit::CheckRemotePresence => 9927,
            Exit::CreateRemoteFolder => 9928,
            Exit::WrongRemoteFolderString => 9929,
            Exit::ScriptFailedWithCode(ecode) => ecode,
            Exit::ScriptFailedWithoutCode => 9930,
            Exit::LoadArgumentsFile => 9931,
            Exit::LoadRemotesFile => 9932,
            Exit::LoadOutputsFile => 9933,
            Exit::SpawnThreadPool => 9934,
            Exit::RemotesExhausted => 9935,
            Exit::OutputsExhausted => 9936,
            Exit::ArgumentsExhausted => 9937,
            Exit::PostProcFailed => 9938,
            Exit::ExecutionSpawnFailed => 9940,
            Exit::SomeExecutionFailed(_) => 9941
        }
    }
}
