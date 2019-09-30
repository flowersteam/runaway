//! liborchestra/asynclets.rs
//!
//! This module contains pieces of async code that can be assembled to produce larger pieces of
//! asynchronous logic.


//------------------------------------------------------------------------------------------ IMPORTS


use std::path::{Path, PathBuf};
use std::fs;
use sha1::{Sha1, Digest};
use crate::primitives::{Local, LocalLocation, File, AbsolutePath, RelativePath, Folder, Located, 
                        Archive, Remote, RemoteLocation, RawCommand, AsResult, local, remote};
use globset;
use std::io::Read;
use std::marker::PhantomData;


//-------------------------------------------------------------------------------------------- TYPES

#[derive(Debug, PartialEq, Eq)]
pub struct Glob<S: AsRef<str>>(S);


//---------------------------------------------------------------------------------------- ASYNCLETS


/// Reads globs specs from a file
pub fn read_globs_from_file<I: AsRef<Path>>
                           (file: Local<File<AbsolutePath<I>>>)
                           -> Result<Vec<Glob<String>>, String>{
    // We unpack the file
    let Located{content:File(AbsolutePath(file_path)), ..} = file;

    // We read the file
    let mut cnt = String::new();
    let mut  file = std::fs::File::open(file_path).map_err(|e| format!("Failed to open glob file: {}", e))?;
    file.read_to_string(&mut cnt).map_err(|e| format!("Failed to read blog file: {}", e))?;

    // We extract the globs
    let globs = cnt.lines()
        .map(|e| Glob(e.to_owned()))
        .collect();

    Ok(globs)
}

/// List a local folder. Returns paths to every files relative to the root. Ignore globs can be 
/// along with include_globs, which supersede the ignore globs.
pub fn list_local_folder<'a,              // Lifetime of the root reference 
                               R: AsRef<Path>>  // A unit type representing the root. 
                              (root: Local<Folder<AbsolutePath<&'a R>>>, 
                               ignore_globs: Vec<Glob<String>>,
                               include_globs: Vec<Glob<String>>) 
                               -> Result<Vec<Local<File<RelativePath<&'a R, PathBuf>>>>, String>{
    
    // We unpack the root
    let Located{content: Folder(AbsolutePath(root)), ..} = root;

    // We create ignore globset
    let mut ignore = globset::GlobSetBuilder::new();
    ignore_globs.iter()
        .map(|Glob(e)| globset::Glob::new(root.as_ref().join(e).to_str().unwrap()))
        .collect::<Result<Vec<globset::Glob>, globset::Error>>()
        .map_err(|e| format!("Failed to parse ignore glob: {}", e))?
        .into_iter()
        .for_each(|g| {ignore.add(g);});
    let ignore = ignore.build().map_err(|e| format!("Failed to build ignore: {}", e))?;

    // We create include globset
    let mut include = globset::GlobSetBuilder::new();
    include_globs.iter()
        .map(|Glob(e)| globset::Glob::new(root.as_ref().join(e).to_str().unwrap()))
        .collect::<Result<Vec<globset::Glob>, globset::Error>>()
        .map_err(|e| format!("Failed to parse include glob: {}", e))?
        .into_iter()
        .for_each(|g| {include.add(g);});
    let include = include.build().map_err(|e| format!("Failed to build include: {}", e))?;

    // We list files
    walkdir::WalkDir::new(root.as_ref())
        .into_iter()
        .filter_map(|e| e.ok())
        .map(|p| p.path().to_owned())
        .filter(|e| e.is_file())
        .filter(|p| !ignore.is_match(p) || include.is_match(p))
        .map(|p| p.strip_prefix(root).unwrap().to_path_buf())
        .map(|path| Ok(Located{location: LocalLocation, content: File(RelativePath{root, path})}))
        .collect()
}


/// Compress a set of local files into a local tar archive.
pub fn tar_local_files<'a,
                        R: AsRef<Path>,
                        P: AsRef<Path>>
                        (files: Vec<Local<File<RelativePath<&'a R, P>>>>, 
                         output: Local<AbsolutePath<P>>) 
                        -> Result<Local<Archive<AbsolutePath<PathBuf>>>, String>{
    // We unpack the argument
    let Located{content: AbsolutePath(output), ..} = output;
    // We create the archive
    let archive_file = fs::File::create(output.as_ref()).unwrap();
    let mut archive = tar::Builder::new(archive_file);
    // We add the files
    files.into_iter()
        // We add files
        .map(|Located{content: File(RelativePath{root, path}), ..}| {
            archive
                .append_file(
                    path.as_ref(),
                    &mut fs::File::open(root.as_ref().join(path.as_ref())).unwrap(),
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
    let hash = compute_local_sha1(Located{location: LocalLocation, 
                                          content:File(AbsolutePath(&output))})?;
    // We return the archive
    Ok(local(Archive{
            file: File(AbsolutePath(output.as_ref().to_path_buf())),
            hash}))
}

/// Computes the sha1 hash of a local file.
pub fn compute_local_sha1<'a, 
                          R: AsRef<Path>>
                          (file: Local<File<AbsolutePath<&'a R>>>) 
                          -> Result<String, String>{
    // We unpack the argument
    let Located{content: File(AbsolutePath(path)), ..} = file;
    // We read the file
    let mut file = fs::File::open(path).map_err(|_| "Failed to open file".to_string())?;
    let mut hasher = Sha1::new();
    std::io::copy(&mut file, &mut hasher).map_err(|_| "Failed to compute hash".to_string())?;
    // We compute the hash
    Ok(format!("{:x}", hasher.result()))
}


// Uncompresses a tar achive into a set of files.
pub fn untar_local_archive<'a,               // Lifetime of root 
                                 R: AsRef<Path>,   // Root unit type
                                 P: AsRef<Path>>   // Archive path type
                                 (archive: Local<Archive<AbsolutePath<P>>>,
                                  root: Local<Folder<AbsolutePath<&'a R>>>)
                                 -> Result<Vec<Local<File<RelativePath<&'a R, PathBuf>>>>, String> {
    // We unpack the root 
    let Located{content: Folder(AbsolutePath(root)), ..} = root;
    let Located{content: Archive{file: File(AbsolutePath(archive)), ..}, ..} = archive;
    // We open the archive and unpacks it
    let arch_file = fs::File::open(archive.as_ref())
        .map_err(|e| format!("Failed to open archive: {}", e))?;
    let mut archive = tar::Archive::new(arch_file);
    // We unpack
    archive.unpack(root.as_ref())
        .map_err(|e| format!("Failed to unpack archive: {}", e))?;
    // We list files
    list_local_folder(Located{location: LocalLocation, content:Folder(AbsolutePath(root))}, 
        vec!(), 
        vec!())
} 

// Moves a local file to a remote file.
pub async fn send_local_file<F: AsRef<Path>,
                             T: AsRef<Path>>
                             (from: Local<File<AbsolutePath<F>>>,
                              to: Remote<AbsolutePath<T>>)
                             -> Result<Remote<File<AbsolutePath<T>>>,String>{
    // We unpack args
    let Located{content: File(AbsolutePath(from)), ..}  = from;
    let Located{content: AbsolutePath(to), location: RemoteLocation(node)} = to;
    // We check if file already exists on the remote end
    let already_there = node.async_exec(format!("test -f {}", to.as_ref().to_str().unwrap()).into())
        .await
        .map_err(|e| format!("Failed to test file: {}", e))?;
    // Depending on the result, we send the file
    if already_there.status.success() {
        return Err("File already exists".into())
    } else {
        node.async_scp_send(from.as_ref().to_path_buf(), 
                            to.as_ref().to_path_buf())
            .await
            .map_err(|e| format!("Failed to send input data: {}", e))?;
    }
    Ok(remote(File(AbsolutePath(to)), node))
}

// Fetches a remote file 
pub async fn fetch_remote_file<F: AsRef<Path>,
                               T: AsRef<Path>>
                               (from: Remote<File<AbsolutePath<F>>>,
                                to: Local<AbsolutePath<T>>)
                               -> Result<Local<File<AbsolutePath<T>>>,String>{
    // We unpack args
    let Located{content: File(AbsolutePath(from)), location: RemoteLocation(node)} = from; 
    let Located{content: AbsolutePath(to), ..}  = to;
    // We check if file already exists
    if to.as_ref().exists(){
        return Err("File already exists".into())
    } 
    // Depending on the result, we fetch the file
    node.async_scp_fetch(from.as_ref().into(), 
                         to.as_ref().into())
        .await
        .map_err(|e| format!("Failed to fetch file: {}", e))?;
    Ok(local(File(AbsolutePath(to))))
}


//  Lists the files in a remote folder.
pub async fn list_remote_folder<'a,             // Lifetime of the root reference 
                               R: AsRef<Path>>  // A unit type representing the root. 
                              (root: Remote<Folder<AbsolutePath<&'a R>>>, 
                               ignore_globs: Vec<Glob<String>>,
                               include_globs: Vec<Glob<String>>) 
                               -> Result<Vec<Remote<File<RelativePath<&'a R, PathBuf>>>>, String>{
    
    // We unpack the root
    let Located{content: Folder(AbsolutePath(root)), location: RemoteLocation(node)} = root;

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
    let output = node.async_exec(format!("find {} -type f", root.as_ref().to_str().unwrap()).into())
        .await
        .map_err(|e| format!("Failed to list remote files: {}", e))?;
    String::from_utf8(output.stdout).unwrap()
        .lines()
        .map(PathBuf::from)
        .map(|l| l.strip_prefix(root).unwrap().to_path_buf())
        .filter(|p| !ignore.is_match(p) || include.is_match(p))
        .map(|path| Ok(remote(File(RelativePath{root, path}), node.clone())))
        .collect()
}


/// Compress a set of remote files into a local tar archive.
pub async fn tar_remote_files<'a,
                             R: AsRef<Path>,
                             P: AsRef<Path>>
                             (files: Vec<Remote<File<RelativePath<&'a R, P>>>>, 
                              output: Remote<AbsolutePath<P>>) 
                              -> Result<Remote<Archive<AbsolutePath<PathBuf>>>, String>{
    // We unpack the argument
    let Located{content: AbsolutePath(output), location: RemoteLocation(node)} = output;
    let root = files.get(0).unwrap().content.0.root.as_ref();
    // We create the files string
    let files_string = files.iter()
        .fold(String::new(), |mut s, Located{content: File(RelativePath{path, ..}), ..}| {
            s.push_str(path.as_ref().to_str().unwrap());
            s.push(' ');
            s
        });
    // We create the archive
    let command = RawCommand(format!("cd {} && tar -cvf {} {}", 
        root.to_str().unwrap(), 
        output.as_ref().to_str().unwrap(),
        files_string));
    node.async_exec(command)
        .await
        .map_err(|e| format!("Failed to execute tar command: {}", e))?
        .result()
        .map_err(|e| format!("Failed to tar files: {}", e))?;
    // We compute the hash
    let hash = compute_remote_sha1(remote(File(AbsolutePath(&output)), node.clone()))
        .await?;
    // We return the archive
    Ok(remote(Archive{file: File(AbsolutePath(output.as_ref().into())), hash}, node.clone()))
}

/// Compress a set of remote files into a local tar archive.
pub async fn compute_remote_sha1<'a,
                                 P: AsRef<Path>>
                                (file: Remote<File<AbsolutePath<&'a P>>>)
                                -> Result<String, String>{
    // We unpack the argument
    let Located{content: File(AbsolutePath(path)), location: RemoteLocation(node)} = file;
    // We compute the hash
    let output = node.async_exec(format!("sha1sum {}", path.as_ref().to_str().unwrap()).into())
        .await
        .map_err(|e| format!("Failed to execute sha1sum command: {}", e))?;
    if !output.status.success(){
        return Err("Failed to compute sha1 hash".into())
    }
    Ok(String::from_utf8(output.stdout)
        .unwrap()
        .split(' ')
        .nth(0)
        .unwrap()
        .to_owned())
}


// Uncompresses a tar achive into a set of files.
pub async fn untar_remote_archive<'a,               // Lifetime of root 
                                  R: AsRef<Path>,   // Root unit type
                                  P: AsRef<Path>>   // Archive path type
                                  (archive: Remote<Archive<AbsolutePath<P>>>,
                                   root: Remote<Folder<AbsolutePath<&'a R>>>)
                                  -> Result<Vec<Remote<File<RelativePath<&'a R, PathBuf>>>>, String> {
    // We unpack the root 
    let Located{content: Folder(AbsolutePath(root)), location:RemoteLocation(node)} = root;
    let Located{content: Archive{file: File(AbsolutePath(archive)), ..}, ..} = archive;
    // We open the archive and unpacks it
    let command = RawCommand(format!("tar -xvf {} -C {}", 
        archive.as_ref().to_str().unwrap(), 
        root.as_ref().to_str().unwrap()));
    node.async_exec(command)
        .await
        .map_err(|e| format!("Failed to execute tar command: {}", e))?
        .result()
        .map_err(|e| format!("Failed to untar files: {}", e))?;
    // We read the entries of the archive
    list_remote_folder(remote(Folder(AbsolutePath(root)), node), vec![], vec![]).await
}


//-------------------------------------------------------------------------------------------- TESTS

#[cfg(test)]
mod tests {

    use super::*;
    use shells::wrap_sh;
    use futures::executor::block_on;
    use std::collections::BTreeSet;
    use std::iter::FromIterator;
    use crate::ssh::{config::SshProfile, RemoteHandle};
    
    #[test]
    fn test_read_globs_from_file(){
        wrap_sh!("echo '*.gz\nfolder/*' > /tmp/test_glob").unwrap();
        let globs = read_globs_from_file(
                Located{location: LocalLocation, content: File(AbsolutePath(PathBuf::from("/tmp/test_glob")))}
            ).unwrap();
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
        let files = list_local_folder(Located{location: LocalLocation, content: Folder(AbsolutePath(&root))},
                Vec::new(),
                Vec::new()
            ).unwrap();
        assert_eq!(files.len(), 3);
        assert!(files.contains(&local(File(RelativePath{root: &root, path: PathBuf::from("1")}))));
        assert!(files.contains(&local(File(RelativePath{root: &root, path: PathBuf::from("3")}))));
        assert!(files.contains(&local(File(RelativePath{root: &root, path: PathBuf::from("folder/2")}))));
        // With ignore
        let files = list_local_folder(Located{location: LocalLocation, content: Folder(AbsolutePath(&root))},
                vec!(Glob("folder/*".into())),
                Vec::new()
            ).unwrap();
        assert_eq!(files.len(), 2);
        assert!(files.contains(&local(File(RelativePath{root: &root, path: PathBuf::from("1")}))));
        assert!(files.contains(&local(File(RelativePath{root: &root, path: PathBuf::from("3")}))));
        //With include
        let files = list_local_folder(Located{location: LocalLocation, content: Folder(AbsolutePath(&root))},
                vec!(Glob("folder/*".into())),
                vec!(Glob("**/2".into()))
            ).unwrap();
        assert_eq!(files.len(), 3);
        assert!(files.contains(&local(File(RelativePath{root: &root, path: PathBuf::from("1")}))));
        assert!(files.contains(&local(File(RelativePath{root: &root, path: PathBuf::from("3")}))));
        assert!(files.contains(&local(File(RelativePath{root: &root, path: PathBuf::from("folder/2")}))));
        wrap_sh!("rm -rf /tmp/test_dir").unwrap();
    }

    #[test]
    fn test_tar_untar_local_file(){
        wrap_sh!("mkdir /tmp/test_dir && mkdir /tmp/test_dir/folder").unwrap();
        wrap_sh!("touch /tmp/test_dir/1 && touch /tmp/test_dir/folder/2 && touch /tmp/test_dir/3").unwrap();
        wrap_sh!("mkdir /tmp/test_dir2").unwrap();
        let root_path = PathBuf::from("/tmp/test_dir");
        let root = &root_path;
        let location = LocalLocation;
        let files = vec![
            Located{location, content: File(RelativePath{root, path:"1".into()})},
            Located{location, content: File(RelativePath{root, path:"3".into()})},
            Located{location, content: File(RelativePath{root, path:"folder/2".into()})},
        ];
        let output = Located{location, content: AbsolutePath("/tmp/test_archive")};
        let tar_archive = tar_local_files(files, output).unwrap();
        assert!((tar_archive.content.file.0).0.exists());
        let shell_sha1 = wrap_sh!("sha1sum /tmp/test_archive").unwrap();
        let sha1 = shell_sha1.split(' ').nth(0).unwrap();
        assert_eq!(sha1, tar_archive.content.hash);

        let root_path = PathBuf::from("/tmp/test_dir2");
        let new_root = Located{location, content: Folder(AbsolutePath(&root_path))};
        let root = &root_path;
        let files = untar_local_archive(tar_archive, new_root).unwrap();
        let expected_files = vec![
            Located{location, content: File(RelativePath{root, path:"1".into()})},
            Located{location, content: File(RelativePath{root, path:"3".into()})},
            Located{location, content: File(RelativePath{root, path:"folder/2".into()})},
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
        let loc_file = local(File(AbsolutePath(PathBuf::from("/tmp/test_file"))));
        let rem_loc = remote(AbsolutePath(PathBuf::from("/tmp/test_file2")), node.clone());
        block_on(send_local_file(loc_file, rem_loc)).unwrap();
        assert!(PathBuf::from("/tmp/test_file2").exists());
        let rem_file = remote(File(AbsolutePath(PathBuf::from("/tmp/test_file2"))), node.clone());
        let loc_loc = local(AbsolutePath(PathBuf::from("/tmp/test_file3")));
        block_on(fetch_remote_file(rem_file, loc_loc)).unwrap();
        assert!(PathBuf::from("/tmp/test_file3").exists());
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
        let files = block_on(list_remote_folder(remote(Folder(AbsolutePath(&root)), node.clone()),
                Vec::new(),
                Vec::new()
            )).unwrap();
        assert_eq!(files.len(), 3);
        assert!(files.contains(&remote(File(RelativePath{root: &root, path: PathBuf::from("1")}), node.clone())));
        assert!(files.contains(&remote(File(RelativePath{root: &root, path: PathBuf::from("3")}), node.clone())));
        assert!(files.contains(&remote(File(RelativePath{root: &root, path: PathBuf::from("folder/2")}), node.clone())));
        // With ignore
        let files = block_on(list_remote_folder(remote(Folder(AbsolutePath(&root)), node.clone()),
                vec!(Glob("folder/*".into())),
                Vec::new()
            )).unwrap();
        assert_eq!(files.len(), 2);
        assert!(files.contains(&remote(File(RelativePath{root: &root, path: PathBuf::from("1")}), node.clone())));
        assert!(files.contains(&remote(File(RelativePath{root: &root, path: PathBuf::from("3")}), node.clone())));
        //With include
        let files = block_on(list_remote_folder(remote(Folder(AbsolutePath(&root)), node.clone()),
                vec!(Glob("folder/*".into())),
                vec!(Glob("**/2".into()))
            )).unwrap();
        assert_eq!(files.len(), 3);
        assert!(files.contains(&remote(File(RelativePath{root: &root, path: PathBuf::from("1")}), node.clone())));
        assert!(files.contains(&remote(File(RelativePath{root: &root, path: PathBuf::from("3")}), node.clone())));
        assert!(files.contains(&remote(File(RelativePath{root: &root, path: PathBuf::from("folder/2")}), node.clone())));
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
        let root_path = PathBuf::from("/tmp/test_dir");
        let root = &root_path;
        let files = vec![
            remote(File(RelativePath{root, path:"1".into()}), node.clone()),
            remote(File(RelativePath{root, path:"3".into()}), node.clone()),
            remote(File(RelativePath{root, path:"folder/2".into()}), node.clone()),
        ];
        let output = remote(AbsolutePath("/tmp/test_archive"), node.clone());
        let tar_archive = block_on(tar_remote_files(files, output)).unwrap();
        assert!((tar_archive.content.file.0).0.exists());
        let shell_sha1 = wrap_sh!("sha1sum /tmp/test_archive").unwrap();
        let sha1 = shell_sha1.split(' ').nth(0).unwrap();
        assert_eq!(sha1, tar_archive.content.hash);

        let root_path = PathBuf::from("/tmp/test_dir2");
        let new_root = remote(Folder(AbsolutePath(&root_path)), node.clone());
        let root = &root_path;
        let files = block_on(untar_remote_archive(tar_archive, new_root)).unwrap();
        let expected_files = vec![
            remote(File(RelativePath{root, path:"1".into()}), node.clone()),
            remote(File(RelativePath{root, path:"3".into()}), node.clone()),
            remote(File(RelativePath{root, path:"folder/2".into()}), node.clone()),
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
