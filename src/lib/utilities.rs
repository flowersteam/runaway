// liborchestra/utilities.rs
// Author: Alexandre Péré
///
/// Some utilities meant for private use of the library.

// IMPORTS
use std::{str, path, fs, process, io::prelude::*};
use crypto;
use crypto::digest::Digest;
use super::Error;
use super::{DATA_RPATH, SEND_ARCH_RPATH, SEND_IGNORE_RPATH};

// FUNCTIONS
/// Allows to run a system command represented by `args` as a process, under a given directory
/// `cmd_dir`. An string `action` explaining the command must be provided for logging purpose.
/// Depending on `capture_streams` the command streams are either piped captured or outputted to the
/// program streams.
pub fn run_command(args: Vec<&str>, cmd_dir: &path::PathBuf, action: &str) -> Result<process::Output, Error> {
    debug!("Start to {}", action);
    // We split the arguments
    let (command, arguments) = args.split_first().unwrap();
    // We perform the command
    let output = process::Command::new(command)
        .args(arguments)
        .current_dir(cmd_dir)
        .stdin(process::Stdio::inherit())
        .stdout(process::Stdio::piped())
        .stderr(process::Stdio::piped())
        .output()?;
    // Depending on the output, we return the corresponding
    match output.status.success() {
        true => return Ok(output),
        false => {
            warn!("Failed to {}. Command returned {:?}", action, output);
            return Err(Error::ExecutionFailed(output));
        }
    }
}

/// Allows to generate a message from the streams of an output
pub fn output_to_message(output: &process::Output) -> String {
    return [String::from_utf8(output.stdout.clone()).unwrap_or(String::new()),
        String::from_utf8(output.stderr.clone()).unwrap_or(String::new())].join("\n");
}

/// Writes lfs gitattributes at a given location
pub fn write_lfs_gitattributes(wpath: &path::PathBuf) -> Result<(), Error> {
    debug!("Writing lfs gitattributes in {}", wpath.to_str().unwrap());
    // We append gitattributes file
    let mut gitattributes_file = fs::File::create(wpath.join(".gitattributes"))?;
    writeln!(gitattributes_file, "# Added by expegit. Do not remove the following line").unwrap();
    writeln!(gitattributes_file, "* filter=lfs diff=lfs merge=lfs -text").unwrap();
    writeln!(gitattributes_file, ".gitattributes -filter -diff -merge text").unwrap();
    gitattributes_file.sync_all()?;
    Ok(())
}

/// Writes the execution gitignore at a given location
pub fn write_exc_gitignore(wpath: &path::PathBuf) -> Result<(), Error> {
    debug!("Writing executions gitignore in {}", wpath.to_str().unwrap());
    // We append gitignore file
    let mut gitignore_file = fs::File::create(wpath.join(".gitignore"))?;
    writeln!(gitignore_file, "# Added by expegit. Do not remove the following lines").unwrap();
    writeln!(gitignore_file, "*").unwrap();
    writeln!(gitignore_file, "!.stdout").unwrap();
    writeln!(gitignore_file, "!.stderr").unwrap();
    writeln!(gitignore_file, "!.gitignore").unwrap();
    writeln!(gitignore_file, "!.excconf").unwrap();
    writeln!(gitignore_file, "!{}", DATA_RPATH).unwrap();
    writeln!(gitignore_file, "!{}/*", DATA_RPATH).unwrap();
    writeln!(gitignore_file, "!{}/**/*", DATA_RPATH).unwrap();
    gitignore_file.sync_all()?;
    Ok(())
}

/// Writes a gitkeep file at a given location
pub fn write_gitkeep(wpath: &path::PathBuf) -> Result<(), Error> {
    debug!("Writing executions gitkeep in {}", wpath.to_str().unwrap());
    // We append gitkeep file
    let mut gitkeep_file = fs::File::create(wpath.join(".gitkeep"))?;
    writeln!(gitkeep_file, "# Added by expegit. Do not remove.").unwrap();
    gitkeep_file.sync_all()?;
    Ok(())
}

/// Returns the current hostname
pub fn get_hostname() -> Result<String, Error> {
    debug!("Retrieving hostname");
    // We retrieve hostname
    let user = str::replace(&String::from_utf8(process::Command::new("id")
        .arg("-u")
        .arg("-n")
        .output()?
        .stdout)
        .unwrap(), "\n", "");
    let host = str::replace(&String::from_utf8(process::Command::new("hostname")
        .output()?
        .stdout)
        .unwrap(), "\n", "");
    Ok(format!("{}@{}", user, host))
}

/// Execute a given command on a remote host using ssh.
pub fn execute_command_on_remote(ssh_config: &str, command: &str, capture_streams: bool) -> Result<process::Output, Error> {
    debug!("Executing command on {}", ssh_config);
    // We retrieve the stdio function
    let stdio_func = match capture_streams {
        true => process::Stdio::piped,
        false => process::Stdio::inherit,
    };
    // We perform the command
    let output = process::Command::new("ssh")
        .arg(ssh_config)
        .arg(command)
        .stdin(stdio_func())
        .stdout(stdio_func())
        .stderr(stdio_func())
        .output()?;
    // Depending on the output, we return the corresponding
    match output.status.success() {
        true => return Ok(output),
        false => {
            warn!("Remote execution failed. Command returned {:?}", output);
            return Err(Error::ExecutionFailed(output));
        }
    }
}

/// Pack the files of a directory into a `.tar` archive
pub fn pack_directory(dir_path: &path::PathBuf) -> Result<(), Error> {
    debug!("Packing directory {}", dir_path.to_str().unwrap());
    // We perform the command
    let tar_output = process::Command::new("tar")
        .arg("cf")
        .arg(SEND_ARCH_RPATH)
        .arg("-X")
        .arg(SEND_IGNORE_RPATH)
        .args(dir_path.read_dir().unwrap().into_iter().map(|x| x.unwrap().file_name()).collect::<Vec<_>>())
        .current_dir(dir_path)
        .output()?;
    // Depending on the output, we return the corresponding
    match tar_output.status.success() {
        true => return Ok(()),
        false => {
            warn!("Packing of directory failed. Command returned {:?}", tar_output);
            return Err(Error::Packing);
        }
    }
}

/// Unpack the content of a `.tar` archive into a directory.
pub fn unpack_archive(archive_path: &path::PathBuf) -> Result<(), Error> {
    debug!("Unpacking archive {}", archive_path.to_str().unwrap());
    // We perform the command
    let tar_output = process::Command::new("tar")
        .arg("xf")
        .arg(archive_path.to_str().unwrap())
        .current_dir(archive_path.parent().unwrap())
        .output()?;
    // Depending on the output, we return the corresponding
    match tar_output.status.success() {
        true => return Ok(()),
        false => {
            warn!("Unpacking of archive failed. Command returned {:?}", tar_output);
            return Err(Error::Unpacking);
        }
    }
}

/// Computes SHA2-256 hash of a file
pub fn compute_file_hash(file_path: &path::PathBuf) -> Result<String, Error> {
    debug!("Computing hash of {}", file_path.to_str().unwrap());
    // We open the file
    let mut file = fs::File::open(file_path)?;
    // We instantiate hasher
    let mut hasher = crypto::sha2::Sha256::new();
    // We read file
    let mut file_content = Vec::new();
    file.read_to_end(&mut file_content)?;
    // We hash
    hasher.input(&file_content);
    // We return
    return Ok(hasher.result_str());
}

/// Sends a file to a remote host
pub fn send_file(local_file_path: &path::PathBuf, ssh_config: &str, host_file_path: &path::PathBuf) -> Result<(), Error> {
    debug!("Sending file {} to {}:{}", local_file_path.to_str().unwrap(), ssh_config, host_file_path.to_str().unwrap());
    // We perform the command
    let scp_output = process::Command::new("scp")
        .arg(local_file_path.to_str().unwrap())
        .arg(format!("{}:{}", ssh_config, host_file_path.to_str().unwrap()))
        .output()?;
    // Depending on the output, we return the corresponding
    match scp_output.status.success() {
        true => return Ok(()),
        false => {
            warn!("Sending file failed. Command returned: {:?}", scp_output);
            return Err(Error::ScpSend);
        }
    }
}

/// Fetch a file from a remote host
pub fn fetch_file(local_file_path: &path::PathBuf, ssh_config: &str, host_file_path: &path::PathBuf) -> Result<(), Error> {
    debug!("Fetching file {}:{} to {}", ssh_config, host_file_path.to_str().unwrap(), local_file_path.to_str().unwrap());
    // We perform the command
    let scp_output = process::Command::new("scp")
        .arg(format!("{}:{}", ssh_config, host_file_path.to_str().unwrap()))
        .arg(local_file_path.to_str().unwrap())
        .output()?;
    // Depending on the output, we return the corresponding
    match scp_output.status.success() {
        true => return Ok(()),
        false => {
            warn!("Fetching file failed. Command returned {:?}", scp_output);
            return Err(Error::ScpFetch);
        }
    }
}


#[cfg(test)]
mod test {
    use std::fs;
    use std::path;
    use std::collections::HashSet;
    use std::iter::FromIterator;
    use super::*;

    // Modify the files with the variables that suits your setup to run the test.
    static TEST_PATH: &str = include_str!("../../test/constants/test_path");
    static TEST_HOSTNAME: &str = include_str!("../../test/constants/test_hostname");

    #[test]
    fn test_write_lfs_gitattributes() {
        let test_path = path::PathBuf::from(TEST_PATH).join("liborchestra/utilities/write_lfs_gitattributes");
        if !test_path.exists() {
            fs::create_dir_all(&test_path).unwrap();
        }
        if test_path.join(".gitattributes").exists() {
            fs::remove_file(test_path.join(".gitattributes")).unwrap();
        }
        write_lfs_gitattributes(&test_path).unwrap();
        assert!(test_path.join(".gitattributes").exists());
    }

    #[test]
    fn test_write_exc_gitignore() {
        let test_path = path::PathBuf::from(TEST_PATH).join("liborchestra/utilities/write_exc_gitignore");
        if !test_path.exists() {
            fs::create_dir_all(&test_path).unwrap();
        }
        if test_path.join(".gitignore").exists() {
            fs::remove_file(test_path.join(".gitignore")).unwrap();
        }
        write_exc_gitignore(&test_path).unwrap();
        assert!(test_path.join(".gitignore").exists());
    }

    #[test]
    fn test_write_gitkeep() {
        let test_path = path::PathBuf::from(TEST_PATH).join("liborchestra/utilities/write_exc_gitkeep");
        if !test_path.exists() {
            fs::create_dir_all(&test_path).unwrap();
        }
        if test_path.join(".gitkeep").exists() {
            fs::remove_file(test_path.join(".gitkeep")).unwrap();
        }
        write_gitkeep(&test_path).unwrap();
        assert!(test_path.join(".gitkeep").exists());
    }

    #[test]
    fn test_get_hostname() {
        let hostname = get_hostname().unwrap();
        assert_eq!(hostname, TEST_HOSTNAME);
    }

    #[test]
    fn test_execute_remote_command() {
        let test_path = path::PathBuf::from(TEST_PATH).join("liborchestra/utilities/hostprofile");
        if test_path.join("touch.md").exists() {
            fs::remove_file(test_path.join("touch.md"));
        }
        execute_command_on_remote("localhost", format!("cd {} && touch touch.md", test_path.to_str().unwrap()).as_str(), false);
        assert!(test_path.join("touch.md").exists());
        let output = execute_command_on_remote("localhost", "echo stdout && >&2 echo stderr ", true).unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert_eq!(stdout.as_str(), "stdout\n");
        assert_eq!(stderr.as_str(), "stderr\n");
    }

    #[test]
    fn test_pack_unpack() {
        let test_path = path::PathBuf::from(TEST_PATH).join("liborchestra/utilities/pack_unpack");
        if test_path.join("recovered").exists() {
            fs::remove_dir_all(test_path.join("recovered"));
        }
        fs::create_dir(test_path.join("recovered"));
        pack_directory(&test_path.join("original")).expect("error");
        fs::copy(test_path.join("original/.send.tar"), test_path.join("recovered/.send.tar")).unwrap();
        unpack_archive(&test_path.join("recovered/.send.tar")).unwrap();
        assert!(test_path.join("recovered/1.md").exists());
        assert!(test_path.join("recovered/2.md").exists());
        assert!(!test_path.join("recovered/ignored/3.md").exists());
    }

    #[test]
    fn test_hash() {
        let test_path = path::PathBuf::from(TEST_PATH).join("liborchestra/utilities/hash");
        let hash = compute_file_hash(&test_path.join("file")).unwrap();
        assert_eq!(hash, "32a5bd248fcbe43faf368367176e6818b432d418f5ebdf98f1c256371bc6316f");
    }

    #[test]
    fn test_send_fetch() {
        let test_path = path::PathBuf::from(TEST_PATH).join("liborchestra/utilities/send_fetch");
        if test_path.join("local/remote_file").exists() {
            fs::remove_file(test_path.join("local/remote_file")).unwrap();
        }
        if test_path.join("remote/local_file").exists() {
            fs::remove_file(test_path.join("remote/local_file")).unwrap();
        }
        send_file(&test_path.join("local/local_file"), "localhost", &test_path.join("remote/local_file")).unwrap();
        fetch_file(&test_path.join("local/remote_file"), "localhost", &test_path.join("remote/remote_file")).unwrap();
        assert!(test_path.join("local/remote_file").exists());
        assert!(test_path.join("remote/local_file").exists());
    }
}
