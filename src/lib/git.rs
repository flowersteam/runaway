// liborchestra/git.rs
//
// Author: Alexandre Péré
///
/// This module allows to perform various git commands on a repository. Those commands are executed
/// through processes.

// IMPORTS
use std::path;
use std::str;
use super::Error;
use super::utilities;


// FUNCTIONS

/// Clone repository from a url. Also clone submodules.
pub fn clone_remote_repo(git_url: &str, cmd_dir: &path::PathBuf) -> Result<(), Error> {
    match utilities::run_command(vec!["git", "clone", "--recurse-submodule", git_url],
                                 cmd_dir,
                                 format!("clone remote repository {}", git_url).as_str())
        {
            Ok(_) => Ok(()),
            Err(Error::ExecutionFailed(output)) => Err(Error::Git(utilities::output_to_message(&output))),
            Err(_) => Err(Error::Unknown),
        }
}

/// Clone a local repository by making a shallow copy of it.
pub fn clone_local_repo(repo_path: &path::PathBuf, cmd_dir: &path::PathBuf) -> Result<(), Error> {
    match utilities::run_command(vec!["git", "clone", "-ls", repo_path.to_str().unwrap(), "."],
                                 cmd_dir,
                                 format!("clone local repository {}", repo_path.to_str().unwrap()).as_str())
        {
            Ok(_) => Ok(()),
            Err(Error::ExecutionFailed(output)) => Err(Error::Git(utilities::output_to_message(&output))),
            Err(_) => Err(Error::Unknown),
        }
}

/// Get the hash of the commit of the branch tip.
pub fn get_tip(branch: &str, cmd_dir: &path::PathBuf) -> Result<String, Error> {
    match utilities::run_command(vec!["git", "rev-parse", branch],
                                 cmd_dir,
                                 format!("get head in {}", cmd_dir.to_str().unwrap()).as_str())
        {
            Ok(output) => Ok(str::replace(String::from_utf8(output.stdout).unwrap().as_str(), "\n", "")),
            Err(Error::ExecutionFailed(output)) => Err(Error::Git(utilities::output_to_message(&output))),
            Err(_) => Err(Error::Unknown),
        }
}

/// Checkout a particular commit given its hash.
pub fn checkout(commit_hash: &str, cmd_dir: &path::PathBuf) -> Result<(), Error> {
    match utilities::run_command(vec!["git", "checkout", commit_hash],
                                 cmd_dir,
                                 format!("check out to {} in {}", commit_hash, cmd_dir.to_str().unwrap()).as_str())
        {
            Ok(_) => Ok(()),
            Err(Error::ExecutionFailed(output)) => Err(Error::Git(utilities::output_to_message(&output))),
            Err(_) => Err(Error::Unknown),
        }
}

/// Get address of the origin branch
pub fn get_origin_url(cmd_dir: &path::PathBuf) -> Result<String, Error> {
    match utilities::run_command(vec!["git", "remote", "get-url", "origin"],
                                 cmd_dir,
                                 format!("get origin in {}", cmd_dir.to_str().unwrap()).as_str())
        {
            Ok(output) => Ok(str::replace(String::from_utf8(output.stdout).unwrap().as_str(), "\n", "")),
            Err(Error::ExecutionFailed(output)) => Err(Error::Git(utilities::output_to_message(&output))),
            Err(_) => Err(Error::Unknown),
        }
}

/// Add submodule to a repository
pub fn add_submodule(sm_url: &str, name: &str, cmd_dir: &path::PathBuf) -> Result<(), Error> {
    match utilities::run_command(vec!["git", "submodule", "add", sm_url, name],
                                 cmd_dir,
                                 format!("add submodule {} in {}", sm_url, cmd_dir.to_str().unwrap()).as_str())
        {
            Ok(_) => Ok(()),
            Err(Error::ExecutionFailed(output)) => Err(Error::Git(utilities::output_to_message(&output))),
            Err(_) => Err(Error::Unknown),
        }
}

/// Update the submodules of a repository.
pub fn update_submodule(cmd_dir: &path::PathBuf) -> Result<(), Error> {
    match utilities::run_command(vec!["git", "submodule", "update", "--remote", "--merge"],
                                 cmd_dir,
                                 format!("update submodule in {}", cmd_dir.to_str().unwrap()).as_str())
        {
            Ok(_) => Ok(()),
            Err(Error::ExecutionFailed(output)) => Err(Error::Git(utilities::output_to_message(&output))),
            Err(_) => Err(Error::Unknown),
        }
}

/// Get list of files staged for commits
pub fn get_staged_files(cmd_dir: &path::PathBuf) -> Result<Vec<path::PathBuf>, Error> {
    match utilities::run_command(vec!["git", "diff", "--name-only", "--cached"],
                                 cmd_dir,
                                 format!("get staged files in {}", cmd_dir.to_str().unwrap()).as_str())
        {
            Ok(output) => Ok(String::from_utf8(output.stdout)
                .unwrap()
                .split('\n')
                .map(|x| cmd_dir.join(path::PathBuf::from(x))).collect()),
            Err(Error::ExecutionFailed(output)) => Err(Error::Git(utilities::output_to_message(&output))),
            Err(_) => Err(Error::Unknown),
        }
}

/// Stage a file for commit.
pub fn stage(stage_path: &path::PathBuf, cmd_dir: &path::PathBuf) -> Result<(), Error> {
    match utilities::run_command(vec!["git", "add", stage_path.to_str().unwrap()],
                                 cmd_dir,
                                 format!("stage {} in {}", stage_path.to_str().unwrap(), cmd_dir.to_str().unwrap()).as_str())
        {
            Ok(_) => Ok(()),
            Err(Error::ExecutionFailed(output)) => Err(Error::Git(utilities::output_to_message(&output))),
            Err(_) => Err(Error::Unknown),
        }
}

/// Commit staged files with a given message.
pub fn commit(commit_message: &str, cmd_dir: &path::PathBuf) -> Result<(), Error> {
    match utilities::run_command(vec!["git", "commit", "-m", commit_message],
                                 cmd_dir,
                                 format!("commit {} in {}", commit_message, cmd_dir.to_str().unwrap()).as_str())
        {
            Ok(_) => Ok(()),
            Err(Error::ExecutionFailed(output)) => Err(Error::Git(utilities::output_to_message(&output))),
            Err(_) => Err(Error::Unknown),
        }
}

/// Push changes to remote. Add an option over `set_upstream` if you push for the first time.
pub fn push(set_upstream: Option<&str>, cmd_dir: &path::PathBuf) -> Result<(), Error> {
    match set_upstream {
        Some(remote) => {
            match utilities::run_command(vec!["git", "push", "-u", remote, "--all"],
                                         cmd_dir,
                                         format!("push to {} in {}", remote, cmd_dir.to_str().unwrap()).as_str())
                {
                    Ok(_) => Ok(()),
                    Err(Error::ExecutionFailed(output)) => Err(Error::Git(utilities::output_to_message(&output))),
                    Err(_) => Err(Error::Unknown),
                }
        }
        None => {
            match utilities::run_command(vec!["git", "push"],
                                         cmd_dir,
                                         format!("push in {}", cmd_dir.to_str().unwrap()).as_str())
                {
                    Ok(_) => Ok(()),
                    Err(Error::ExecutionFailed(output)) => Err(Error::Git(utilities::output_to_message(&output))),
                    Err(_) => Err(Error::Unknown),
                }
        }
    }
}

/// Abort previous rebase
pub fn abort_rebase(cmd_dir: &path::PathBuf) -> Result<(), Error> {
    match utilities::run_command(vec!["git", "rebase", "--abort"],
                                 cmd_dir,
                                 format!("abort rebase in {}", cmd_dir.to_str().unwrap()).as_str())
        {
            Ok(_) => Ok(()),
            Err(Error::ExecutionFailed(output)) => Err(Error::Git(utilities::output_to_message(&output))),
            Err(_) => Err(Error::Unknown),
        }
}

/// Pull changes from remote. Note that submodules are not affected by this command.
pub fn pull(cmd_dir: &path::PathBuf) -> Result<(), Error> {
    match utilities::run_command(vec!["git", "pull", "--rebase", "--autostash", "--no-edit", "origin", "master"],
                                 cmd_dir,
                                 format!("pull in {}", cmd_dir.to_str().unwrap()).as_str())
        {
            Ok(_) => Ok(()),
            Err(Error::ExecutionFailed(output)) => Err(Error::Git(utilities::output_to_message(&output))),
            Err(_) => Err(Error::Unknown),
        }
}

/// Fetch changes from a given branch.
pub fn fetch(origin: &str, cmd_dir: &path::PathBuf) -> Result<(), Error> {
    match utilities::run_command(vec!["git", "fetch", origin],
                                 cmd_dir,
                                 format!("fetch {} in {}", origin, cmd_dir.to_str().unwrap()).as_str())
        {
            Ok(_) => Ok(()),
            Err(Error::ExecutionFailed(output)) => Err(Error::Git(utilities::output_to_message(&output))),
            Err(_) => Err(Error::Unknown),
        }
}

/// Get the hash of the last commit of a branch.
pub fn get_last_commit(branch: &str, cmd_dir: &path::PathBuf) -> Result<String, Error> {
    match utilities::run_command(vec!["git", "log", "-n", "1", "--pretty=format:\"%H\"", branch],
                                 cmd_dir,
                                 format!("get last commit of {} in {}", branch, cmd_dir.to_str().unwrap()).as_str())
        {
            Ok(output) => Ok(str::replace(String::from_utf8(output.stdout).unwrap().as_str(), "\"", "")),
            Err(Error::ExecutionFailed(output)) => Err(Error::Git(utilities::output_to_message(&output))),
            Err(_) => Err(Error::Unknown),
        }
}

/// Get all commits hash.
pub fn get_all_commits(cmd_dir: &path::PathBuf) -> Result<Vec<String>, Error> {
    match utilities::run_command(vec!["git", "log", "--pretty=format:\"%H\""],
                                 cmd_dir,
                                 format!("get all commits in {}", cmd_dir.to_str().unwrap()).as_str())
        {
            Ok(output) => Ok(String::from_utf8(output.stdout)
                .unwrap()
                .split('\n')
                .map(|x| str::replace(x, "\"", ""))
                .collect()),
            Err(Error::ExecutionFailed(output)) => Err(Error::Git(utilities::output_to_message(&output))),
            Err(_) => Err(Error::Unknown),
        }
}

/// Clean a folder from untracked files and directories
pub fn clean(cmd_dir: &path::PathBuf) -> Result<(), Error> {
    match utilities::run_command(vec!["git", "clean", "-dxf"],
                                 cmd_dir,
                                 format!("clean in {}", cmd_dir.to_str().unwrap()).as_str())
        {
            Ok(_) => Ok(()),
            Err(Error::ExecutionFailed(output)) => Err(Error::Git(utilities::output_to_message(&output))),
            Err(_) => Err(Error::Unknown),
        }
}

/// Add a remote to the repository.
pub fn add_remote(name: &str, url: &str, cmd_dir: &path::PathBuf) -> Result<(), Error> {
    match utilities::run_command(vec!["git", "remote", "add", name, url],
                                 cmd_dir,
                                 format!("adding remote {} {} in {}", name, url, cmd_dir.to_str().unwrap()).as_str())
        {
            Ok(_) => Ok(()),
            Err(Error::ExecutionFailed(output)) => Err(Error::Git(utilities::output_to_message(&output))),
            Err(_) => Err(Error::Unknown),
        }
}

/// Initialize a local repository at the given location.
pub fn init(cmd_dir: &path::PathBuf) -> Result<(), Error> {
    match utilities::run_command(vec!["git", "init"],
                                 cmd_dir,
                                 format!("initialize in {}", cmd_dir.to_str().unwrap()).as_str())
        {
            Ok(_) => Ok(()),
            Err(Error::ExecutionFailed(output)) => Err(Error::Git(utilities::output_to_message(&output))),
            Err(_) => Err(Error::Unknown),
        }
}

// TESTS
#[cfg(test)]
mod test {
    use std::fs;
    use uuid;
    use super::*;

    // Modify the files with the variables that suits your setup to run the test.
    static TEST_PATH: &str = env!("ORCHESTRA_TEST_PATH");
    static SIMPLE_REPOSITORY_NAME: &str = env!("ORCHESTRA_TEST_SIMPLE_REPOSITORY_NAME");
    static SIMPLE_REPOSITORY_URL: &str = env!("ORCHESTRA_TEST_SIMPLE_REPOSITORY_URL");
    static SIMPLE_REPOSITORY_HEAD: &str = env!("ORCHESTRA_TEST_SIMPLE_REPOSITORY_HEAD");
    static SIMPLE_REPOSITORY_COMMIT: &str = env!("ORCHESTRA_TEST_SIMPLE_REPOSITORY_COMMIT");
    static PUSH_PULL_REPOSITORY_NAME: &str = env!("ORCHESTRA_TEST_PUSH_PULL_REPOSITORY_NAME");
    static PUSH_PULL_REPOSITORY_URL: &str = env!("ORCHESTRA_TEST_PUSH_PULL_REPOSITORY_URL");

    #[test]
    fn test_clone_remote_repo() {
        let test_path = path::PathBuf::from(TEST_PATH).join("liborchestra/git/clone_remote_repo");
        if !test_path.exists() {
            fs::create_dir_all(&test_path);
        }
        fs::remove_dir_all(test_path.join(SIMPLE_REPOSITORY_NAME));
        clone_remote_repo(SIMPLE_REPOSITORY_URL, &test_path)
            .expect("Failed to clone remote repository");
        assert!(test_path.join(SIMPLE_REPOSITORY_NAME).join("file.md").exists());
    }

    #[test]
    fn test_clone_local_repo() {
        let test_path = path::PathBuf::from(TEST_PATH).join("liborchestra/git/clone_local_repo");
        if !test_path.exists() {
            fs::create_dir_all(&test_path);
        }
        if test_path.join(SIMPLE_REPOSITORY_NAME).exists() {
            fs::remove_dir_all(test_path.join(SIMPLE_REPOSITORY_NAME)).unwrap();
        }
        clone_remote_repo(SIMPLE_REPOSITORY_URL, &test_path)
            .expect("Failed to clone remote repository");
        if test_path.join("local").exists() {
            fs::remove_dir_all(test_path.join("local"));
        }
        fs::create_dir(test_path.join("local")).expect("Failed to create directory");
        clone_local_repo(&test_path.join(SIMPLE_REPOSITORY_NAME), &test_path.join("local")).expect("Failed to clone local repository");
        assert!(test_path.join("local").join("file.md").exists());
    }

    #[test]
    fn test_get_tip() {
        let test_path = path::PathBuf::from(TEST_PATH).join("liborchestra/git/get_head");
        if !test_path.exists() {
            fs::create_dir_all(&test_path);
        }
        if test_path.join(SIMPLE_REPOSITORY_NAME).exists() {
            fs::remove_dir_all(test_path.join(SIMPLE_REPOSITORY_NAME)).unwrap();
        }
        clone_remote_repo(SIMPLE_REPOSITORY_URL, &test_path)
            .expect("Failed to clone remote repository");
        let head = get_tip("HEAD", &test_path.join(SIMPLE_REPOSITORY_NAME)).expect("Failed to get head");
        assert_eq!(head, SIMPLE_REPOSITORY_HEAD);
    }

    #[test]
    fn test_checkout() {
        let test_path = path::PathBuf::from(TEST_PATH).join("liborchestra/git/checkout");
        if !test_path.exists() {
            fs::create_dir_all(&test_path);
        }
        if test_path.join(SIMPLE_REPOSITORY_NAME).exists() {
            fs::remove_dir_all(test_path.join(SIMPLE_REPOSITORY_NAME)).unwrap();
        }
        clone_remote_repo(SIMPLE_REPOSITORY_URL, &test_path)
            .expect("Failed to clone remote repository");
        let head = get_tip("HEAD", &test_path.join(SIMPLE_REPOSITORY_NAME)).expect("Failed to get head");
        assert_eq!(head, SIMPLE_REPOSITORY_HEAD);
        checkout(SIMPLE_REPOSITORY_COMMIT, &test_path.join(SIMPLE_REPOSITORY_NAME)).expect("Failed to reset to commit");
        let head = get_tip("HEAD", &test_path.join(SIMPLE_REPOSITORY_NAME)).expect("Failed to get head after reset");
        assert_eq!(head, SIMPLE_REPOSITORY_COMMIT);
    }


    #[test]
    fn test_get_origin_url() {
        let test_path = path::PathBuf::from(TEST_PATH).join("liborchestra/git/get_origin_url");
        if !test_path.exists() {
            fs::create_dir_all(&test_path);
        }
        if test_path.join(SIMPLE_REPOSITORY_NAME).exists() {
            fs::remove_dir_all(test_path.join(SIMPLE_REPOSITORY_NAME)).unwrap();
        }
        clone_remote_repo(SIMPLE_REPOSITORY_URL, &test_path).expect("Failed to clone remote repository");
        let url = get_origin_url(&test_path.join(SIMPLE_REPOSITORY_NAME)).expect("Failed to get url");
        assert_eq!(url, SIMPLE_REPOSITORY_URL);
    }


    #[test]
    fn test_add_submodule() {
        let test_path = path::PathBuf::from(TEST_PATH).join("liborchestra/git/add_submodule");
        if !test_path.exists() {
            fs::create_dir_all(&test_path);
        }
        if test_path.join(SIMPLE_REPOSITORY_NAME).exists() {
            fs::remove_dir_all(test_path.join(SIMPLE_REPOSITORY_NAME)).unwrap();
        }
        clone_remote_repo(SIMPLE_REPOSITORY_URL, &test_path).expect("Failed to clone remote repository");
        add_submodule(SIMPLE_REPOSITORY_URL, "xprp", &test_path.join(SIMPLE_REPOSITORY_NAME)).expect("Failed to add submodule");
        assert!(test_path.join(SIMPLE_REPOSITORY_NAME).join("xprp").join("file.md").exists());
    }

    #[test]
    fn test_stage_get_staged_files() {
        let test_path = path::PathBuf::from(TEST_PATH).join("liborchestra/git/stage_get_staged_files");
        if !test_path.exists() {
            fs::create_dir_all(&test_path);
        }
        if test_path.join(SIMPLE_REPOSITORY_NAME).exists() {
            fs::remove_dir_all(test_path.join(SIMPLE_REPOSITORY_NAME)).unwrap();
        }
        clone_remote_repo(SIMPLE_REPOSITORY_URL, &test_path).expect("Failed to clone remote repository");
        let staged_files = get_staged_files(&test_path.join(SIMPLE_REPOSITORY_NAME)).unwrap();
        assert_eq!(staged_files.len(), 1);
        fs::File::create(test_path.join(SIMPLE_REPOSITORY_NAME).join("touch.md")).expect("Failed to create file");
        stage(&path::PathBuf::from("."), &test_path.join(SIMPLE_REPOSITORY_NAME)).expect("Failed to stage files");
        let staged_files = get_staged_files(&test_path.join(SIMPLE_REPOSITORY_NAME)).unwrap();
        assert_eq!(staged_files.len(), 2);
        assert!(staged_files.contains(&test_path.join(SIMPLE_REPOSITORY_NAME).join("touch.md")));
    }


    #[test]
    fn test_git_commit() {
        let test_path = path::PathBuf::from(TEST_PATH).join("liborchestra/git/commit");
        if !test_path.exists() {
            fs::create_dir_all(&test_path);
        }
        if test_path.join(SIMPLE_REPOSITORY_NAME).exists() {
            fs::remove_dir_all(test_path.join(SIMPLE_REPOSITORY_NAME)).unwrap();
        }
        clone_remote_repo(SIMPLE_REPOSITORY_URL, &test_path).expect("Failed to clone remote repository");
        fs::File::create(test_path.join(SIMPLE_REPOSITORY_NAME).join("touch.md")).expect("Failed to create file");
        let before_head = get_tip("HEAD", &test_path.join(SIMPLE_REPOSITORY_NAME)).expect("Failed get head");
        stage(&path::PathBuf::from("."), &test_path.join(SIMPLE_REPOSITORY_NAME)).expect("Failed to track");
        let before_staged_files = get_staged_files(&test_path.join(SIMPLE_REPOSITORY_NAME)).unwrap();
        commit("Test Commit", &test_path.join(SIMPLE_REPOSITORY_NAME)).expect("Failed to commit");
        let after_head = get_tip("HEAD", &test_path.join(SIMPLE_REPOSITORY_NAME)).unwrap();
        let after_staged_files = get_staged_files(&test_path.join(SIMPLE_REPOSITORY_NAME)).unwrap();
        assert_ne!(before_head, after_head);
        assert_ne!(before_staged_files, after_staged_files);
        assert!(before_staged_files.contains(&test_path.join(SIMPLE_REPOSITORY_NAME).join("touch.md")));
        assert!(!after_staged_files.contains(&test_path.join(SIMPLE_REPOSITORY_NAME).join("touch.md")));
    }

    #[test]
    fn test_git_push_pull() {
        let test_path = path::PathBuf::from(TEST_PATH).join("liborchestra/git/push_pull");
        if !test_path.exists() {
            fs::create_dir_all(&test_path).unwrap();
            fs::create_dir_all(&test_path.join("1")).unwrap();
            fs::create_dir_all(&test_path.join("2")).unwrap();
        }
        if test_path.join("1").join(PUSH_PULL_REPOSITORY_NAME).exists() {
            fs::remove_dir_all(test_path.join("1").join(PUSH_PULL_REPOSITORY_NAME)).unwrap();
        }
        if test_path.join("2").join(PUSH_PULL_REPOSITORY_NAME).exists() {
            fs::remove_dir_all(test_path.join("2").join(PUSH_PULL_REPOSITORY_NAME)).unwrap();
        }
        clone_remote_repo(PUSH_PULL_REPOSITORY_URL, &test_path.join("1")).expect("Failed to clone remote repository");
        clone_remote_repo(PUSH_PULL_REPOSITORY_URL, &test_path.join("2")).expect("Failed to clone remote repository");
        let my_uuid = uuid::Uuid::new_v4();
        fs::File::create(test_path.join("1")
            .join(PUSH_PULL_REPOSITORY_NAME)
            .join(format!("{}", my_uuid))).expect("Failed to create file");
        stage(&path::PathBuf::from("."), &test_path.join("1").join(PUSH_PULL_REPOSITORY_NAME)).unwrap();
        let head_before_commit_1 = get_tip("HEAD", &test_path.join("1").join(PUSH_PULL_REPOSITORY_NAME)).unwrap();
        commit("Test Commit", &test_path.join("1").join(PUSH_PULL_REPOSITORY_NAME)).unwrap();
        let head_after_commit_1 = get_tip("HEAD", &test_path.join("1").join(PUSH_PULL_REPOSITORY_NAME)).unwrap();
        push(None, &test_path.join("1").join(PUSH_PULL_REPOSITORY_NAME)).expect("Failed to push");
        let head_before_pull_2 = get_tip("HEAD", &test_path.join("2").join(PUSH_PULL_REPOSITORY_NAME)).unwrap();
        pull(&test_path.join("2").join(PUSH_PULL_REPOSITORY_NAME)).expect("Failed to pull");
        let head_after_pull_2 = get_tip("HEAD", &test_path.join("2").join(PUSH_PULL_REPOSITORY_NAME)).unwrap();
        assert_ne!(head_before_commit_1, head_after_commit_1);
        assert_eq!(head_before_commit_1, head_before_pull_2);
        assert_eq!(head_after_commit_1, head_after_pull_2);
    }

    #[test]
    fn test_get_last_commit() {
        let test_path = path::PathBuf::from(TEST_PATH).join("liborchestra/git/get_last_commit");
        if !test_path.exists() {
            fs::create_dir_all(&test_path);
        }
        if test_path.join(SIMPLE_REPOSITORY_NAME).exists() {
            fs::remove_dir_all(test_path.join(SIMPLE_REPOSITORY_NAME)).unwrap();
        }
        clone_remote_repo(SIMPLE_REPOSITORY_URL, &test_path).expect("Failed to clone remote repository");
        fs::File::create(test_path.join(SIMPLE_REPOSITORY_NAME).join("touch.md")).expect("Failed to create file");
        let before_last_commit = get_last_commit("master", &test_path.join(SIMPLE_REPOSITORY_NAME)).expect("Failed to get comit before");
        stage(&path::PathBuf::from("."), &test_path.join(SIMPLE_REPOSITORY_NAME)).expect("Failed to track");
        commit("Test Commit", &test_path.join(SIMPLE_REPOSITORY_NAME)).expect("Failed to commit");
        let after_last_commit = get_last_commit("master", &test_path.join(SIMPLE_REPOSITORY_NAME)).expect("Failed to get commit after");
        assert_ne!(before_last_commit, after_last_commit);
        checkout(before_last_commit.as_str(), &test_path.join(SIMPLE_REPOSITORY_NAME)).expect("failed to checkout");
        let after_after_last_commit = get_last_commit("master", &test_path.join(SIMPLE_REPOSITORY_NAME)).unwrap();
        assert_eq!(after_last_commit, after_after_last_commit);
    }

    #[test]
    fn test_get_all_commits_hashs() {
        let test_path = path::PathBuf::from(TEST_PATH).join("liborchestra/git/get_all_commits");
        if !test_path.exists() {
            fs::create_dir_all(&test_path);
        }
        if test_path.join(SIMPLE_REPOSITORY_NAME).exists() {
            fs::remove_dir_all(test_path.join(SIMPLE_REPOSITORY_NAME)).unwrap();
        }
        clone_remote_repo(SIMPLE_REPOSITORY_URL, &test_path).expect("Failed to clone remote repository");
        fs::File::create(test_path.join(SIMPLE_REPOSITORY_NAME).join("touch.md")).expect("Failed to create file");
        let before_last_commit = get_last_commit("master", &test_path.join(SIMPLE_REPOSITORY_NAME)).expect("Failed to get comit before");
        stage(&path::PathBuf::from("."), &test_path.join(SIMPLE_REPOSITORY_NAME)).expect("Failed to track");
        commit("Test Commit", &test_path.join(SIMPLE_REPOSITORY_NAME)).expect("Failed to commit");
        let after_last_commit = get_last_commit("master", &test_path.join(SIMPLE_REPOSITORY_NAME)).expect("Failed to get commit after");
        let all_commits = get_all_commits(&test_path.join(SIMPLE_REPOSITORY_NAME)).unwrap();
        assert!(all_commits.contains(&before_last_commit));
        assert!(all_commits.contains(&after_last_commit));
        checkout(all_commits.get(0).unwrap(), &test_path.join(SIMPLE_REPOSITORY_NAME)).expect("failed to checkout");
        let after_after_last_commit = get_last_commit("master", &test_path.join(SIMPLE_REPOSITORY_NAME)).unwrap();
        let all_commits = get_all_commits(&test_path.join(SIMPLE_REPOSITORY_NAME)).unwrap();
        assert!(all_commits.contains(&before_last_commit));
        assert!(all_commits.contains(&after_last_commit));
        assert!(all_commits.contains(&after_after_last_commit));
    }

    #[test]
    fn test_clean() {
        let test_path = path::PathBuf::from(TEST_PATH).join("liborchestra/git/clean");
        if !test_path.exists() {
            fs::create_dir_all(&test_path);
        }
        if test_path.join(SIMPLE_REPOSITORY_NAME).exists() {
            fs::remove_dir_all(test_path.join(SIMPLE_REPOSITORY_NAME)).unwrap();
        }
        clone_remote_repo(SIMPLE_REPOSITORY_URL, &test_path).expect("Failed to clone remote repository");
        fs::File::create(test_path.join(SIMPLE_REPOSITORY_NAME).join("touch.md")).expect("Failed to create file");
        assert!(test_path.join(SIMPLE_REPOSITORY_NAME).join("touch.md").exists());
        clean(&test_path.join(SIMPLE_REPOSITORY_NAME)).unwrap();
        assert!(!test_path.join(SIMPLE_REPOSITORY_NAME).join("touch.md").exists());
    }
}
