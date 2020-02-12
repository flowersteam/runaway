//! This module contains primitives functions (asynchronous or not) that can be assembled to produce
//! larger pieces of logic commonly used in the applications.


//------------------------------------------------------------------------------------------ IMPORTS


use std::path::{PathBuf};
use std::fs;
use sha1::{Sha1, Digest};
use crate::commons::{AsResult, RawCommand};
use crate::ssh::RemoteHandle;
use globset;
use std::io::Read;
use std::fmt;
use tracing::{self, instrument};



//-------------------------------------------------------------------------------------------- TYPES

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct Glob<S: AsRef<str>>(pub S);
impl<S: AsRef<str>> From<S> for Glob<String>{
    fn from(other: S) -> Self{
        Glob(other.as_ref().to_owned())
    }
}

#[derive(Clone, PartialEq)]
pub struct Sha1Hash(String);
impl From<Sha1Hash> for String{
    fn from(other: Sha1Hash) -> String{
        other.0.clone()
    }
}
impl fmt::Display for Sha1Hash{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}


//---------------------------------------------------------------------------------------- ASYNCLETS


/// Reads globs specs from a file
#[instrument]
pub fn read_globs_from_file(file: &PathBuf) -> Result<Vec<Glob<String>>, String>{

    // We read the file
    let mut cnt = String::new();
    let mut  file = std::fs::File::open(file).map_err(|e| format!("Failed to open glob file: {}", e))?;
    file.read_to_string(&mut cnt).map_err(|e| format!("Failed to read blog file: {}", e))?;

    // We extract the globs
    let globs = cnt.lines()
        .map(|e| Glob(e.to_owned()))
        .collect();

    Ok(globs)

}

/// List a local folder. Returns paths to every files relative to the root. Ignore globs can be
/// along with include_globs, which supersede the ignore globs.
#[instrument]
pub fn list_local_folder(root: &PathBuf,
                         ignore_globs: &Vec<Glob<String>>,
                         include_globs: &Vec<Glob<String>>)
                        -> Result<Vec<PathBuf>, String>{

    // We create ignore globset
    let mut ignore = globset::GlobSetBuilder::new();
    ignore_globs.iter()
        .map(|Glob(e)| globset::Glob::new(root.join(e).to_str().unwrap()))
        .collect::<Result<Vec<globset::Glob>, globset::Error>>()
        .map_err(|e| format!("Failed to parse ignore glob: {}", e))?
        .into_iter()
        .for_each(|g| {ignore.add(g);});
    let ignore = ignore.build().map_err(|e| format!("Failed to build ignore: {}", e))?;

    // We create include globset
    let mut include = globset::GlobSetBuilder::new();
    include_globs.iter()
        .map(|Glob(e)| globset::Glob::new(root.join(e).to_str().unwrap()))
        .collect::<Result<Vec<globset::Glob>, globset::Error>>()
        .map_err(|e| format!("Failed to parse include glob: {}", e))?
        .into_iter()
        .for_each(|g| {include.add(g);});
    let include = include.build().map_err(|e| format!("Failed to build include: {}", e))?;

    // We list files
    walkdir::WalkDir::new(root)
        .into_iter()
        .filter_map(|e| e.ok())
        .map(|p| p.path().to_owned())
        .filter(|e| e.is_file())
        .filter(|p| !ignore.is_match(p) || include.is_match(p))
        .map(|p| p.strip_prefix(&root).unwrap().to_path_buf())
        .map(|path| Ok(path))
        .collect()

}


/// Compress a set of local files into a local tar archive.
#[instrument]
pub fn tar_local_files(root: &PathBuf,
                       files: &Vec<PathBuf>,
                       output: &PathBuf)
                      -> Result<Sha1Hash, String>{

    // We create the archive
    let archive_file = fs::File::create(output).unwrap();
    let mut archive = tar::Builder::new(archive_file);
    // We add the files
    files.into_iter()
        // We add files
        .map(|path| {
            archive
                .append_file(
                    path,
                    &mut fs::File::open(root.join(path)).unwrap(),
                )
                .map_err(|e| format!("Failed to add file to the archive: {}", e))
        })
        // We check if any error occurred
        .collect::<Result<Vec<_>, String>>()?;
    // We close the archive
    archive
        .finish()
        .map_err(|e| format!("Failed to finish the archive: {}", e))?;
    // We compute hash
    compute_local_sha1(output)

}

/// Computes the sha1 hash of a local file.
#[instrument]
pub fn compute_local_sha1(file: &PathBuf) -> Result<Sha1Hash, String>{

    // We read the file
    let mut file = fs::File::open(file).map_err(|_| "Failed to open file".to_string())?;
    let mut hasher = Sha1::new();
    std::io::copy(&mut file, &mut hasher).map_err(|_| "Failed to compute hash".to_string())?;
    // We compute the hash
    Ok(Sha1Hash(format!("{:x}", hasher.result())))

}


// Uncompresses a tar achive into a set of files.
#[instrument]
pub fn untar_local_archive(archive: &PathBuf, root: &PathBuf) -> Result<Vec<PathBuf>, String> {

    // We open the archive and unpacks it
    let arch_file = fs::File::open(archive)
        .map_err(|e| format!("Failed to open archive: {}", e))?;
    // We list files
    let files = tar::Archive::new(arch_file)
        .entries()
        .map_err(|e| format!("Failed to read archive entries: {}", e))?
        .map(|e| e.unwrap().path().unwrap().into_owned())
        .collect();
    // We unpack
    let arch_file = fs::File::open(archive)
        .map_err(|e| format!("Failed to open archive: {}", e))?;
    tar::Archive::new(arch_file)
        .unpack(root)
        .map_err(|e| format!("Failed to unpack archive: {}", e))?;
    Ok(files)
}

// Moves a local file to a remote file.
#[instrument]
pub async fn send_local_file(from: &PathBuf, to: &PathBuf, node: &RemoteHandle)-> Result<(),String>{

    // We check if file already exists on the remote end
    let already_there = node.async_exec(format!("test -f {}", to.to_str().unwrap()).into())
        .await
        .map_err(|e| format!("Failed to test file: {}", e))?;
    // Depending on the result, we send the file
    if already_there.status.success() {
        return Err("File already exists".into())
    } else {
        node.async_scp_send(from.to_path_buf(),
                            to.to_path_buf())
            .await
            .map_err(|e| format!("Failed to send input data: {}", e))?;
    }
    Ok(())

}

// Fetches a remote file
#[instrument]
pub async fn fetch_remote_file(from: &PathBuf, to: &PathBuf, node: &RemoteHandle) -> Result<(),String>{

    // We check if file already exists
    //if to.exists(){
    //    return Err("File already exists".into())
    //}
    // Depending on the result, we fetch the file
    node.async_scp_fetch(from.to_owned(), to.to_owned())
        .await
        .map_err(|e| format!("Failed to fetch file: {}", e))?;
    Ok(())

}


//  Lists the files in a remote folder.
#[instrument]
pub async fn list_remote_folder(root: &PathBuf,
                                ignore_globs: &Vec<Glob<String>>,
                                include_globs: &Vec<Glob<String>>,
                                node: &RemoteHandle)
                               -> Result<Vec<PathBuf>, String>{
    // We create ignore globset
    let mut ignore = globset::GlobSetBuilder::new();
    ignore_globs.iter()
        .map(|Glob(e)| globset::Glob::new(e))
        .collect::<Result<Vec<globset::Glob>, globset::Error>>()
        .map_err(|e| format!("Failed to parse ignore glob: {}", e))?
        .into_iter()
        .for_each(|g| {ignore.add(g);});
    let ignore = ignore.build().map_err(|e| format!("Failed to build ignore: {}", e))?;

    // We create include globset
    let mut include = globset::GlobSetBuilder::new();
    include_globs.iter()
        .map(|Glob(e)| globset::Glob::new(e))
        .collect::<Result<Vec<globset::Glob>, globset::Error>>()
        .map_err(|e| format!("Failed to parse include glob: {}", e))?
        .into_iter()
        .for_each(|g| {include.add(g);});
    let include = include.build().map_err(|e| format!("Failed to build include: {}", e))?;

    // We list all files in dir
    let output = node.async_exec(format!("find {} -type f", root.to_str().unwrap()).into())
        .await
        .map_err(|e| format!("Failed to list remote files: {}", e))?;
    String::from_utf8(output.stdout).unwrap()
        .lines()
        .map(PathBuf::from)
        .map(|l| l.strip_prefix(root).unwrap().to_path_buf())
        .filter(|p| !ignore.is_match(p) || include.is_match(p))
        .map(|path| Ok(path))
        .collect()
}


/// Compress a set of remote files into a local tar archive.
#[instrument]
pub async fn tar_remote_files (root: &PathBuf,
                               files: &Vec<PathBuf>,
                               output: &PathBuf,
                               node: &RemoteHandle)
                              -> Result<Sha1Hash, String>{

    // We create the files string
    let files_string;
    if files.is_empty(){
        files_string = format!("--files-from /dev/null");
    } else {
        files_string = files.iter()
        .fold(String::new(), |mut s, path| {
            s.push_str(path.to_str().unwrap());
            s.push(' ');
            s
        });
    }
    // We create the archive
    let command = RawCommand(format!("cd {} && tar -cvf {} {}",
        root.to_str().unwrap(),
        output.to_str().unwrap(),
        files_string));
    node.async_exec(command)
        .await
        .map_err(|e| format!("Failed to execute tar command: {}", e))?
        .result()
        .map_err(|e| format!("Failed to tar files: {}", e))?;
    // We compute the hash
    compute_remote_sha1(output, node).await

}

/// Compress a set of remote files into a local tar archive.
#[instrument]
pub async fn compute_remote_sha1(file: &PathBuf, node: &RemoteHandle)
        -> Result<Sha1Hash, String>{

    // We compute the hash
    let output = node.async_exec(format!("sha1sum {}", file.to_str().unwrap()).into())
        .await
        .map_err(|e| format!("Failed to execute sha1sum command: {}", e))?;
    if !output.status.success(){
        return Err("Failed to compute sha1 hash".into())
    }
    Ok(Sha1Hash(String::from_utf8(output.stdout)
        .unwrap()
        .split(' ')
        .nth(0)
        .unwrap()
        .to_owned()))
}


// Uncompresses a tar achive into a set of files.
#[instrument]
pub async fn untar_remote_archive(archive: &PathBuf, root: &PathBuf, node: &RemoteHandle)
        -> Result<Vec<PathBuf>, String> {

    // We open the archive and unpacks it
    let command = RawCommand(format!("tar -xvf {} -C {}",
        archive.to_str().unwrap(),
        root.to_str().unwrap()));
    node.async_exec(command)
        .await
        .map_err(|e| format!("Failed to execute tar command: {}", e))?
        .result()
        .map_err(|e| format!("Failed to untar files: {}", e))?;
    // We read the entries of the archive
    let none = vec!();
    list_remote_folder(root, &none, &none, node).await

}

// Checks whether a file exists or not.
#[instrument]
pub async fn remote_file_exists(file: &PathBuf, node: &RemoteHandle) -> Result<bool, String> {

    let command = RawCommand(format!("test -f {}",
        file.to_str().unwrap()));
    Ok(node.async_exec(command)
        .await
        .map_err(|e| format!("Failed to execute check command: {}", e))?
        .status
        .success())

}


// Checks whether a file exists or not.
#[instrument]
pub async fn remote_folder_exists(folder: &PathBuf, node: &RemoteHandle) -> Result<bool, String> {

    let command = RawCommand(format!("test -d {}",
        folder.to_str().unwrap()));
    Ok(node.async_exec(command)
        .await
        .map_err(|e| format!("Failed to execute check command: {}", e))?
        .status
        .success())

}


// Removes a set of files
#[instrument]
pub async fn remove_remote_files(files: Vec<PathBuf>, node: &RemoteHandle) -> Result<(), String> {

    let command = RawCommand(files.iter()
        .fold("rm ".to_string(), |acc, f| format!("{} {}", acc, f.to_str().unwrap())));
    node.async_exec(command)
        .await
        .map_err(|e| format!("Failed to execute remove command: {}", e))?
        .result()
        .map_err(|e| format!("Failed to remove files: {}", e))?;
    Ok(())
}


// Removes a set of files
#[instrument]
pub async fn remove_remote_folder(folder: PathBuf, node: &RemoteHandle) -> Result<(), String> {

    let command = RawCommand(format!("rm -rf {}", folder.to_str().unwrap()));
    node.async_exec(command)
        .await
        .map_err(|e| format!("Failed to execute remove command: {}", e))?
        .result()
        .map_err(|e| format!("Failed to remove folder: {}", e))?;
    Ok(())
}

// Creates a folder
#[instrument]
pub async fn create_remote_folder(folder: &PathBuf, node: &RemoteHandle) -> Result<(), String> {

    let command = RawCommand(format!("mkdir {}", folder.to_str().unwrap()));
    node.async_exec(command)
        .await
        .map_err(|e| format!("Failed to execute mkdir command: {}", e))?
        .result()
        .map_err(|e| format!("Failed to create folder: {}", e))?;
    Ok(())

}



//-------------------------------------------------------------------------------------------- TESTS

#[cfg(test)]
mod tests {

    use super::*;
    use shells::wrap_sh;
    use futures::executor::block_on;
    use crate::ssh::{config::SshProfile, RemoteHandle};

    #[test]
    fn test_read_globs_from_file(){
        wrap_sh!("echo '*.gz\nfolder/*' > /tmp/test_glob").unwrap();
        let globs = read_globs_from_file(&PathBuf::from("/tmp/test_glob")).unwrap();
        assert_eq!(globs.len(), 2);
        assert_eq!(&Glob("*.gz".into()), globs.get(0).unwrap());
        assert_eq!(&Glob("folder/*".into()), globs.get(1).unwrap());
        wrap_sh!("rm /tmp/test_glob").unwrap();
    }

    #[test]
    fn test_list_local_folder() {
        wrap_sh!("mkdir /tmp/test_dir && mkdir /tmp/test_dir/folder").unwrap();
        wrap_sh!("touch /tmp/test_dir/1 && touch /tmp/test_dir/folder/2 && touch /tmp/test_dir/3").unwrap();
        let root = PathBuf::from("/tmp/test_dir");
        let files = list_local_folder(&root,
                &Vec::new(),
                &Vec::new()).unwrap();
        assert_eq!(files.len(), 3);
        assert!(files.contains(&PathBuf::from("1")));
        assert!(files.contains(&PathBuf::from("3")));
        assert!(files.contains(&PathBuf::from("folder/2")));
        // With ignore
        let files = list_local_folder(&root,
            &vec!(Glob("folder/*".into())),
            &Vec::new()).unwrap();
        assert_eq!(files.len(), 2);
        assert!(files.contains(&PathBuf::from("1")));
        assert!(files.contains(&PathBuf::from("3")));
        //With include
        let files = list_local_folder(&root,
                &vec!(Glob("folder/*".into())),
                &vec!(Glob("**/2".into()))
            ).unwrap();
        assert_eq!(files.len(), 3);
        assert!(files.contains(&PathBuf::from("1")));
        assert!(files.contains(&PathBuf::from("3")));
        assert!(files.contains(&PathBuf::from("folder/2")));
        wrap_sh!("rm -rf /tmp/test_dir").unwrap();
    }

    #[test]
    fn test_tar_untar_local_file(){
        wrap_sh!("mkdir /tmp/test_dir && mkdir /tmp/test_dir/folder").unwrap();
        wrap_sh!("touch /tmp/test_dir/1 && touch /tmp/test_dir/folder/2 && touch /tmp/test_dir/3").unwrap();
        wrap_sh!("mkdir /tmp/test_dir2").unwrap();
        let root = PathBuf::from("/tmp/test_dir");
        let files = vec![
            "1".into(),
            "3".into(),
            "folder/2".into(),
        ];
        let output = PathBuf::from("/tmp/test_archive");
        let tar_archive = tar_local_files(&root, &files, &output).unwrap();
        assert!(output.exists());
        let shell_sha1 = wrap_sh!("sha1sum /tmp/test_archive").unwrap();
        let sha1 = shell_sha1.split(' ').nth(0).unwrap();
        assert_eq!(sha1, tar_archive.0);

        let root = PathBuf::from("/tmp/test_dir2");
        let files = untar_local_archive(&output, &root).unwrap();
        let expected_files = vec![
            "1".into(),
            "3".into(),
            "folder/2".into(),
        ];
        for a in expected_files.iter(){
            assert!(files.contains(a))
        }
        for a in files.iter(){
            assert!(expected_files.contains(a))
        }
        wrap_sh!("rm -rf /tmp/test_dir /tmp/test_dir2 /tmp/test_archive").unwrap();
    }

    #[test]
    fn test_send_fetch() {
        let profile = SshProfile{
            name: "test".to_owned(),
            hostname: Some("localhost".to_owned()),
            user: Some("apere".to_owned()),
            port: Some(22),
            proxycommand: None // Some("ssh -A -l apere localhost -W localhost:22".to_owned()),
        };
        let node = RemoteHandle::spawn(profile).unwrap();
        wrap_sh!("touch /tmp/test_file").unwrap();
        let local_before = PathBuf::from("/tmp/test_file");
        let remote_then = PathBuf::from("/tmp/test_file2");
        block_on(send_local_file(&local_before, &remote_then, &node)).unwrap();
        assert!(remote_then.exists());
        let local_after = PathBuf::from("/tmp/test_file3");
        block_on(fetch_remote_file(&remote_then, &local_after, &node)).unwrap();
        assert!(local_after.exists());
        wrap_sh!("rm /tmp/test_file*").unwrap();
    }


    #[test]
    fn test_list_remote_folder() {
        let profile = SshProfile{
            name: "test".to_owned(),
            hostname: Some("localhost".to_owned()),
            user: Some("apere".to_owned()),
            port: Some(22),
            proxycommand: None // Some("ssh -A -l apere localhost -W localhost:22".to_owned()),
        };
        let node = RemoteHandle::spawn(profile).unwrap();
        wrap_sh!("mkdir /tmp/test_dir && mkdir /tmp/test_dir/folder").unwrap();
        wrap_sh!("touch /tmp/test_dir/1 && touch /tmp/test_dir/folder/2 && touch /tmp/test_dir/3").unwrap();
        let root = PathBuf::from("/tmp/test_dir");
        let files = block_on(list_remote_folder(&root,
                                                &Vec::new(),
                                                &Vec::new(),
                                                &node)).unwrap();
        assert_eq!(files.len(), 3);
        assert!(files.contains(&PathBuf::from("1")));
        assert!(files.contains(&PathBuf::from("3")));
        assert!(files.contains(&PathBuf::from("folder/2")));
        // With ignore
        let files = block_on(list_remote_folder(&root,
                                                &vec!(Glob("folder/*".into())),
                                                &Vec::new(),
                                                &node)).unwrap();
        assert_eq!(files.len(), 2);
        assert!(files.contains(&PathBuf::from("1")));
        assert!(files.contains(&PathBuf::from("3")));
        //With include
        let files = block_on(list_remote_folder(&root,
                                                &vec!(Glob("folder/*".into())),
                                                &vec!(Glob("**/2".into())),
                                                &node)).unwrap();
        assert_eq!(files.len(), 3);
        assert!(files.contains(&PathBuf::from("1")));
        assert!(files.contains(&PathBuf::from("3")));
        assert!(files.contains(&PathBuf::from("folder/2")));
        wrap_sh!("rm -rf /tmp/test_dir").unwrap();
    }


    #[test]
    fn test_tar_untar_remote_file(){
        let profile = SshProfile{
            name: "test".to_owned(),
            hostname: Some("localhost".to_owned()),
            user: Some("apere".to_owned()),
            port: Some(22),
            proxycommand: None // Some("ssh -A -l apere localhost -W localhost:22".to_owned()),
        };
        let node = RemoteHandle::spawn(profile).unwrap();
        wrap_sh!("mkdir /tmp/test_dir && mkdir /tmp/test_dir/folder").unwrap();
        wrap_sh!("touch /tmp/test_dir/1 && touch /tmp/test_dir/folder/2 && touch /tmp/test_dir/3").unwrap();
        wrap_sh!("mkdir /tmp/test_dir2").unwrap();
        let root = PathBuf::from("/tmp/test_dir");
        let files = vec![
            "1".into(),
            "3".into(),
            "folder/2".into(),
        ];
        let output = PathBuf::from("/tmp/test_archive");
        let tar_archive = block_on(tar_remote_files(&root, &files, &output, &node)).unwrap();
        assert!(output.exists());
        let shell_sha1 = wrap_sh!("sha1sum /tmp/test_archive").unwrap();
        let sha1 = shell_sha1.split(' ').nth(0).unwrap();
        assert_eq!(sha1, tar_archive.0);

        let root = PathBuf::from("/tmp/test_dir2");
        let files = block_on(untar_remote_archive(&output, &root, &node)).unwrap();
        let expected_files = vec![
            "1".into(),
            "3".into(),
            "folder/2".into(),
        ];
        for a in expected_files.iter(){
            assert!(files.contains(a))
        }
        for a in files.iter(){
            assert!(expected_files.contains(a))
        }
        wrap_sh!("rm -rf /tmp/test_dir /tmp/test_dir2 /tmp/test_archive").unwrap();
    }

}
