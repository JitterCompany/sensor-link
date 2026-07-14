use glob;
use std::{
    env, fs,
    io::Write,
    path::{Path, PathBuf},
};

pub fn read_file(name: impl AsRef<Path>) -> String {
    let name = name.as_ref();
    match fs::read_to_string(name) {
        Ok(result) => result,
        Err(err) => {
            panic!("Failed to read '{:?}': {err:?}", name);
        }
    }
}

pub fn write_file(name: impl AsRef<Path>, data: &[u8]) {
    let name = name.as_ref();
    let out_dir = env::var("OUT_DIR").unwrap();
    let file_path = Path::new(&out_dir).join(name);

    let mut out =
        fs::File::create(file_path).unwrap_or_else(|_| panic!("Failed to create '{:?}'", name));
    out.write_all(&data)
        .unwrap_or_else(|_| panic!("Failed to write '{:?}'", name));
}

/// Finds the one file matching path.
/// Panics if multiple files are found.
/// Returns None if no file is found.
pub fn find_file(path: &PathBuf) -> Option<PathBuf> {
    let pattern = path.to_str().unwrap();
    if let Ok(mut matches) = glob::glob(pattern) {
        if let Some(path) = matches.next() {
            // Check if it is the only result
            if matches.next().is_none() {
                return path.ok();
            } else {
                panic!("Multiple files matching pattern: {pattern}");
            }
        }
    }
    None
}
