// liborchestra/misc.rs
//
// Author: Alexandre Péré
///
/// A few miscellaneous functions publicly available.

// IMPORTS
use std::{process, path, fs};
use regex;
use super::Error;
use super::EXPEGIT_RPATH;


// FUNCTIONS

/// Returns a tuple containing the git and git-lfs versions.
pub fn check_git_lfs_versions() -> Result<(String, String), Error> {
    debug!("Checking git and lfs versions");
    let git_version = String::from_utf8(process::Command::new("git")
        .args(&["--version"])
        .output()?.stdout)
        .expect("Failed to parse utf8 string");
    let lfs_version = String::from_utf8(process::Command::new("git")
        .args(&["lfs", "--version"])
        .output()?.stdout)
        .expect("Failed to parse utf8 string");
    let git_regex = regex::Regex::new(r"[0-9]+\.[0-9]+\.[0-9]+")?;
    let git_version = String::from(git_regex.find(&git_version)
        .unwrap()
        .as_str());
    let lfs_regex = regex::Regex::new(r"[0-9]+\.[0-9]+\.[0-9]+")?;
    let lfs_version = String::from(lfs_regex.find(&lfs_version)
        .unwrap()
        .as_str());
    Ok((git_version, lfs_version))
}

/// Returns the absolute path to the higher expegit folder starting from `start_path`.
pub fn search_expegit_root(start_path: &path::PathBuf) -> Result<path::PathBuf, Error> {
    debug!("Searching expegit repository root from {}", fs::canonicalize(start_path).unwrap().to_str().unwrap());
    let start_path = fs::canonicalize(start_path)?;
    if start_path.is_file() { panic!("Should provide a folder path.") };
    // We add a dummy folder that will be popped directly to check for .
    let mut start_path = start_path.join("dummy_folder");
    while start_path.pop() {
        if start_path.join(EXPEGIT_RPATH).exists() {
            debug!("Expegit root found at {}", start_path.to_str().unwrap());
            return Ok(start_path);
        }
    }
    warn!("Unable to find .expegit file in parents folders");
    Err(Error::InvalidRepository)
}

/// Parses a parameters string with a number of repetitions and generate a vector of parameters
/// combinations.
pub fn parse_parameters(param_string: &str, repeats: usize) -> Vec<String> {
    // We compute the products of entered parameters recursively
    fn parameters_generator(p: Vec<&str>, repeat: usize) -> Vec<String> {
        if p.len() == 1 {
            p.first()
                .unwrap()
                .split(';')
                .map::<Vec<String>, _>(|s| {
                    (0..repeat).map(|_| String::from(s.trim())).collect()
                }).flatten()
                .collect()
        } else {
            p.first()
                .unwrap()
                .split(';')
                .map::<Vec<String>, _>(|s| {
                    let mut params =
                        parameters_generator(p.split_first().unwrap().1.to_vec(), repeat);
                    params.iter_mut().for_each(|b| {
                        b.insert(0, ' ');
                        b.insert_str(0, s)
                    });
                    params
                }).flatten()
                .collect()
        }
    }
    parameters_generator(param_string.split("¤").collect(), repeats)
}

// TESTS
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
    fn test_check_git_lfs_versions() {
        check_git_lfs_versions().unwrap();
    }

    #[test]
    fn test_search_expegit_root() {
        let test_path = path::PathBuf::from(TEST_PATH).join("liborchestra/misc/search_expegit_root");
        println!("{:?}", test_path);
        if !test_path.exists() {
            fs::create_dir_all(&test_path).unwrap();
            fs::create_dir_all(&test_path.join("no/1/2")).unwrap();
            fs::create_dir_all(&test_path.join("yes/1/2")).unwrap();
            fs::File::create(&test_path.join("yes/.expegit")).unwrap();
        }
        let no_res = search_expegit_root(&test_path.join("no/1/2"));
        assert!(no_res.is_err());
        let yes_res = search_expegit_root(&test_path.join("yes/1/2"));
        assert!(yes_res.is_ok());
        assert_eq!(yes_res.unwrap(), test_path.join("yes"));
        let yes_res = search_expegit_root(&test_path.join("yes"));
        assert!(yes_res.is_ok());
        assert_eq!(yes_res.unwrap(), test_path.join("yes"));
    }
}