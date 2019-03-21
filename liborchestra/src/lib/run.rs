// liborchestra/run.rs
// Author: Alexandre Péré
///
/// Provides structures to execute a script on a remote host. Basically, all that is inside the folder containing
/// the script to be executed will be sent to the distant machine. This means that the ressources needed for your 
/// computation must exist in the folder. Data that are sent to the remote, and fetch after execution can be parameterized 
/// using the `.sendignore` and `.fetchignore` files. Any file matching a glob in the `.sendignore` will not be 
/// sent to the remote (you may want to put your `.git` folder inside this), and any file matching a glob in the 
/// `.fetchignore` will not be fetched (you may want to put everything but your data and logs in that). If you 
/// put something in your `.fetchignore`, mind to not ignore the `.fetchignore` in the `.sendignore`.

// IMPORTS
use std::{path, process, fs, io, str, io::prelude::*, env, error, fmt};
use crate::error as craterr;
use utilities::*;
use uuid;
use yaml_rust;
use super::{SEND_ARCH_RPATH, PROFILES_FOLDER_RPATH, SEND_IGNORE_RPATH, FETCH_IGNORE_RPATH, FETCH_ARCH_RPATH};

// ERRORS
#[derive(Debug)]
pub enum Error {
    ProfileError,
    Unknown,
}

impl error::Error for Error {}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            ProfileError => write!(f, "The profile is ill formed"),
            Unknown => write!(f, "Unknown error occured"),
        }
    }
}



// STRUCTURES
/// Represents an host on which a run can be executed. Those can be parsed from a `yaml` file following this convention:
/// ```yaml
/// # Name of the ssh config to use. Must be defined in your ~/.ssh/config.
/// ssh_config: localhost
/// 
/// # Path to the host directory in which to put the code.
/// host_directory: ~/Executions
/// 
/// # Bash commands to execute before the script.
/// before_execution:
///   - echo 'Preparing Execution'
///   - echo 'Executed on $HOSTNAME'
/// 
/// # Bash command to execute the script. The following environment variables are replaced at run time:
/// #     + `$SCRIPT_NAME`: the file name of the script.
/// #     + `$SCRIPT_ARGS`: the arguments of the script.
/// execution:
///   - echo 'Starting Execution'
///   - ./$SCRIPT_NAME $SCRIPT_ARGS
/// 
/// # Bash commands to execute after the script.
/// after_execution:
///   - echo 'Cleaning Execution'
/// ```
/// 
/// 
#[derive(Debug)]
struct HostProfile {
    name: String,
    ssh_config: String,
    host_directory: path::PathBuf,
    before_execution: Vec<String>,
    execution: Vec<String>,
    after_execution: Vec<String>,
}

impl HostProfile {

    /// Imports a host configuration from the path of a compatible `yaml` file.
    fn import(yml_path: &path::PathBuf) -> Result<HostProfile, craterr::Error>{
        // We check if the yaml file exists
        if !yml_path.exists(){
            return Err(craterr::Error::from(io::Error::new(io::ErrorKind::NotFound, yml_path.to_str().unwrap().to_owned())))
        }
        // We read the yaml file
        let mut yml_file = fs::File::open(yml_path)?;
        let mut yml_string = String::new();
        yml_file.read_to_string(&mut yml_string)?;
        let yml_struct = &yaml_rust::YamlLoader::load_from_str(yml_string.as_str())?[0];
        // We parse the profile
        let ssh_config = match yml_struct["ssh_config"].as_str(){
            Some(s) => s.to_owned(),
            None => return Err(Error::ProfileError),
        };
        let host_directory = match yml_struct["host_directory"].as_str(){
            Some(s) => path::PathBuf::from(s.to_owned()),
            None => return Err(Error::ProfileError),
        };
        let before_execution:Vec<String> = match yml_struct["before_execution"].as_vec(){
            Some(s) => s.to_owned().into_iter().map(|x| x.as_str().unwrap().to_owned()).collect(),
            None => Vec::new(),
        };
        let execution:Vec<String> = match yml_struct["execution"].as_vec(){
            Some(s) => s.to_owned().into_iter().map(|x| x.as_str().unwrap().to_owned()).collect(),
            None => return Err(Error::ProfileError),
        };
        let after_execution:Vec<String> = match yml_struct["after_execution"].as_vec(){
            Some(s) => s.to_owned().into_iter().map(|x| x.as_str().unwrap().to_owned()).collect(),
            None => Vec::new(),
        };
        // We return the host profile
        Ok(HostProfile{
            name: yml_path.file_name().unwrap().to_str().unwrap().replace(".yml", ""),
            ssh_config,
            host_directory,
            before_execution,
            execution,
            after_execution
        })
    }
    
    /// Returns the execution string.
    fn get_complete_execution_string(&self)-> String{
        let mut complete: Vec<String> = Vec::new();
        complete.extend(self.before_execution.clone());
        complete.extend(self.execution.clone());
        complete.extend(self.after_execution.clone());
        return complete.join(" && ");
    }

    /// Returns the ssh config string.
    fn get_ssh_config(&self) -> String{
        return self.ssh_config.clone();
    }

    /// Returns the host execution directory.
    fn get_host_directory(&self) -> path::PathBuf{
        return self.host_directory.clone();
    }
}

/// This enumeration represents the different things one would want to leave on the remote
/// after a `RunConfig` execution:
/// + `Nothing` removes both the code and the data. This means that the next time the exact same 
/// code would be executed, it would have to be sent again.
/// + `Code` removes only the data from the execution, but leaves the code on the remote. This
/// means that if the same code must be re-executed, it doesn't have to be sent again.
/// + `Everything` leaves both the code and the data from the execution. This means that if the 
/// same code must be re-executed, it doesn't have to be sent again, and the data will still be
/// available on the remote if something goes wrong.
#[derive(Debug)]
pub enum LeaveConfig{
    Nothing,
    Code,
    Everything,
}

impl<'a> From<&'a str> for LeaveConfig{
    fn from(conf: &str) -> LeaveConfig{
        match conf{
            "nothing" => LeaveConfig::Nothing,
            "code" => LeaveConfig::Code,
            "everything" => LeaveConfig::Everything,
            _ => panic!("Unknown LeaveConfig input encountered {}", conf),
        }
    }
}

/// Allows to run a script on a remote host. Example of use:
/// ```rust
/// use std::path;
/// // Run definition
/// let run = RunConfig{
///     script_path: path::PathBuf::from("myscript.sh"),
///     profile: String::from("localhost"),
///     parameters: String::from("-p --environment=1"),
/// };
/// // Execution
/// let output = run.execute(LeaveConfig::Everything, true).unwrap();
/// ```
/// 
/// The `script_path` field must be a valid path to an executable script. The `profile` string must be
/// the name of a valid runaway profile located in your `~/.runaway` directory. The `parameters` string 
/// must be a set of valid parameters for the script.
#[derive(Debug)]
pub struct RunConfig{
    pub script_path: path::PathBuf,
    pub profile: String,
    pub parameters: String,
}

impl RunConfig{

    /// Executes the configuration on the remote host. What goes on under the hood? 
    /// + 1: The profile is loaded from `~/.runaway`
    /// + 2: If no `.sendignore` exists in the script folder, we create one
    /// + 3: If no `.fetchignore` exists in the script folder, we create one
    /// + 4: We compress the files matching the globs in `.sendignore` into a `.send.tar` tar archive
    /// + 5: We compute the hash of the archive
    /// + 5: If no folder under the remote execution directory matches the hash of the archive, we create
    /// it, and send the `.send.tar` archive into it.
    /// + 6: We generate a uuid for the run
    /// + 7: On remote, we unpack the `.send.tar` archive into a folder named after the run uuid.
    /// + 9: On remote, we execute the script under the uuid folder
    /// + 10: On remote, we pack the files matching the globs in `.fetchignore` into a `.fetch.tar` archive.
    /// + 11: From local, we fetch the `.fetch.tar` archive and expand it
    /// + 12: Depending on the leave config, we remove the uuid folder, or the hash folder.
    /// 
    /// The streams from the execution can be piped to the main application streams, or can be captured 
    /// to be returned .
    pub fn execute(&self, leave: LeaveConfig, capture_streams: bool) -> Result<process::Output, Error>{
        info!("Running config {:?}", self);
        // We get some useful values
        let script_path = path::PathBuf::from(&self.script_path).canonicalize()?;
        if !script_path.is_file(){
            warn!("Failed to find the script path to execute.");
            return Err(craterr::Error::from(io::Error::new(io::ErrorKind::NotFound, script_path.to_str().unwrap())));
        }
        let directory_path = script_path.parent().unwrap().to_path_buf();
        let send_archive_path = directory_path.join(SEND_ARCH_RPATH);
        let profile_path = match env::home_dir() {
            Some(path) => path.join(PROFILES_FOLDER_RPATH)
                .join(format!("{}.yml", self.profile)),
            None => {
                warn!("Failed to retrieve the home directory");
                return Err(craterr::Error::from(io::Error::new(io::ErrorKind::NotFound, "~")));
            }
        };
        let profile_path = profile_path.canonicalize()?;
        let host_profile = HostProfile::import(&profile_path)?;
        // If they do not exist, we create sendignore and fetchignore
        if !directory_path.join(SEND_IGNORE_RPATH).exists(){
            let mut sendignore = fs::File::create(directory_path.join(SEND_IGNORE_RPATH)).unwrap();
            write!(sendignore, "# Created by Runaway\n\
            # Files that match globs pattern written here will not be sent to remote host.\n\
            # If you have a .fetchignore, do not ignore it here :)").unwrap();
        }
        if !directory_path.join(FETCH_IGNORE_RPATH).exists(){
            let mut sendignore = fs::File::create(directory_path.join(FETCH_IGNORE_RPATH)).unwrap();
            write!(sendignore, "# Created by Runaway\n\
            # Files that match globs pattern written here will not be fetch from remote host.").unwrap();
        }
        // We pack the files
        match pack_directory(&directory_path){
            Ok(_) => {},
            Err(err) =>{
                warn!("Failed to pack directory: {}", err);
                return Err(err);
            }
        };
        // We retrieve archive hash
        let hash = match compute_file_hash(&send_archive_path){
            Ok(hash) => hash,
            Err(err) => {
                warn!("Failed to get archive hash");
                return Err(err);
            }
        };
        // If hash folder is not found, we send the files
        let host_path = host_profile.get_host_directory().join(hash);
        let identifier = format!("{}", uuid::Uuid::new_v4());
        let host_id_path = host_path.join(identifier);
        let host_sent_tar_path = host_path.join(SEND_ARCH_RPATH);
        let host_script_path = host_id_path.join(script_path.file_name().unwrap());
        let exists = execute_command_on_remote(&host_profile.get_ssh_config(),format!("mkdir {}",host_path.to_str().unwrap()).as_str(), false).is_err();
        if !exists{
            match send_file(&send_archive_path, &host_profile.get_ssh_config(), &host_path){
                Ok(_) => {},
                Err(err) => {
                    warn!("Failed to send files: {}", err);
                    return Err(err);
                }
            }
        }
        // We construct the command
        let command = host_profile.get_complete_execution_string()
            .replace("$SCRIPT_NAME", host_script_path.to_str().unwrap())
            .replace("$SCRIPT_ARGS", self.parameters.as_str());
        let mut complete: Vec<String> = Vec::new();
        complete.push(format!("mkdir {}", host_id_path.to_str().unwrap()));
        complete.push(format!("cd {}", host_id_path.to_str().unwrap()));
        complete.push(format!("tar -xf {} -C {}", host_sent_tar_path.to_str().unwrap(), host_id_path.to_str().unwrap()));
        complete.push(command);
        complete.push(format!("tar -cf {} -X {} *",
                              host_id_path.join(FETCH_ARCH_RPATH).to_str().unwrap(),
                              host_id_path.join(FETCH_IGNORE_RPATH).to_str().unwrap()));
        let complete_command = complete.join(" && ");
        // We execute the command
        let output = match execute_command_on_remote(host_profile.get_ssh_config().as_str(), complete_command.as_str(), capture_streams){
            Ok(output) => output,
            Err(err) => {
                error!("Failed to execute the command on remote: {}", err);
                return Err(err);
            }
        };
        // We fetch the data
        let host_fetchable_tar_path = host_id_path.join(FETCH_ARCH_RPATH);
        let local_fetch_path = directory_path.join(FETCH_ARCH_RPATH);
        match fetch_file(&local_fetch_path, host_profile.get_ssh_config().as_str(), &host_fetchable_tar_path){
            Ok(_) => {},
            Err(err) => {
                warn!("Failed to fetch archive: {}", err);
                return Err(err);
            }
        }
        // We unpack the data
        match unpack_archive(&local_fetch_path){
            Ok(_)=>{},
            Err(err)=>{
                warn!("Failed to unpack data: {}", err);
                return Err(err);
            }
        }
        // Clean up things depending on --leave parameter
        match leave{
            LeaveConfig::Nothing => {
                match execute_command_on_remote(&host_profile.get_ssh_config(),format!("rm -rf {}", host_path.to_str().unwrap()).as_str(), false){
                    Ok(_)=> {},
                    Err(err)=>{
                        warn!("Failed to remove code from remote: {}", err);
                        return Err(err);
                    }
                }
            },
            LeaveConfig::Code => {
                match execute_command_on_remote(&host_profile.get_ssh_config(),format!("rm -rf {}", host_id_path.to_str().unwrap()).as_str(), false){
                    Ok(_)=> {},
                    Err(err)=>{
                        warn!("Failed to remove code from remote: {}", err);
                        return Err(err);
                    }
                }
            },
            LeaveConfig::Everything => { },
        }; 
        // We end the runaway!
        return Ok(output);
    }
}

// TESTS
#[cfg(test)]
mod tests {
    extern crate pretty_logger;
    use super::*;

    // CONSTANTS
    static TEST_PATH: &str = env!("ORCHESTRA_TEST_PATH");

    #[test]
    fn test_host_profile_import() {
        let test_path = path::PathBuf::from(TEST_PATH).join("liborchestra/run/hostprofile");
        let a = HostProfile::import(&test_path.join("localhost.yml")).unwrap();
        assert_eq!(a.name, "localhost");
        assert_eq!(a.ssh_config, "localhost");
        assert_eq!(a.host_directory.to_str().unwrap(), "~/");
        assert_eq!(a.before_execution, vec!["echo Preparing Execution", "echo Executed on $HOSTNAME"]);
        assert_eq!(a.execution, vec!["echo Starting Execution", "./$SCRIPT_NAME $SCRIPT_ARGS"]);
        assert_eq!(a.after_execution, vec!["echo Cleaning Execution"]);

        let a = HostProfile::import(&test_path.join("emptysection.yml")).unwrap();
        assert_eq!(a.name, "emptysection");
        assert_eq!(a.ssh_config, "localhost");
        assert_eq!(a.host_directory.to_str().unwrap(), "~/");
        let empty_vec: Vec<String> = Vec::new();
        assert_eq!(a.before_execution, empty_vec);
        assert_eq!(a.execution, vec!["echo Starting Execution", "./$SCRIPT_NAME $SCRIPT_ARGS"]);
        assert_eq!(a.after_execution, vec!["echo Cleaning Execution"]);
    }

    #[test]
    fn test_host_profile_get_complete_execution_string() {
        let test_path = path::PathBuf::from(TEST_PATH).join("liborchestra/run/hostprofile");
        let a = HostProfile::import(&test_path.join("localhost.yml")).unwrap();
        assert_eq!(a.get_complete_execution_string(), "echo Preparing Execution && echo Executed on $HOSTNAME && \
        echo Starting Execution && ./$SCRIPT_NAME $SCRIPT_ARGS && echo Cleaning Execution");
    }

    #[test]
    fn test_runconfig_execute(){
        let script_path = path::PathBuf::from(TEST_PATH).join("liborchestra/run/runconfig_execute").join("run");
        let runconf = RunConfig{
            script_path,
            parameters: String::from(""),
            profile: String::from("localhost"),
        };
        let output = runconf.execute(LeaveConfig::Everything, true).unwrap();
        assert!(path::PathBuf::from(TEST_PATH).join("liborchestra/run/runconfig_execute").join("run").exists());
        assert_eq!(output.status.code().unwrap(), 0);
    }
}

