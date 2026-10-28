//! Disk is authoritative across MCP processes. Lock a separate stable file,
//! since atomically replacing views.json also replaces its inode.
use anyhow::{Result, anyhow};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    path::Path,
};

type Views = BTreeMap<String, [f64; 2]>;

fn access<T>(path: &Path, write: bool, action: impl FnOnce(&mut Views) -> Result<T>) -> Result<T> {
    crate::metrics::sync_result(if write { "views.persist" } else { "views.read" }, || {
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path.with_extension("lock"))?;
        crate::metrics::sync_result("views.file_lock", || lock.lock())?;
        let mut views = match fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Views::new(),
            Err(error) => return Err(error.into()),
        };
        let result = action(&mut views)?;
        if write {
            let temp = path.with_extension("tmp");
            fs::write(&temp, serde_json::to_vec_pretty(&views)?)?;
            fs::rename(temp, path)?;
        }
        Ok(result)
    })
}

pub fn names(path: &Path) -> Result<Vec<String>> {
    access(path, false, |views| Ok(views.keys().cloned().collect()))
}
pub fn get(path: &Path, name: &str) -> Result<[f64; 2]> {
    access(path, false, |views| {
        views
            .get(name)
            .copied()
            .ok_or_else(|| anyhow!("Unknown saved view: {name}"))
    })
}
pub fn save(path: &Path, name: &str, pose: [f64; 2]) -> Result<()> {
    access(path, true, |views| {
        views.insert(name.to_owned(), pose);
        Ok(())
    })
}
pub fn delete(path: &Path, name: &str) -> Result<Value> {
    access(path, true, |views| {
        Ok(json!({"name":name,"deleted":views.remove(name).is_some()}))
    })
}
pub fn clear(path: &Path) -> Result<Value> {
    access(path, true, |views| {
        let count = views.len();
        views.clear();
        Ok(json!({"cleared":count}))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn independent_writers_observe_deletes_and_cannot_resurrect_cleared_views() {
        let dir = std::env::temp_dir().join(format!("onvif-views-{}", rand::random::<u64>()));
        fs::create_dir(&dir).unwrap();
        let path = dir.join("views.json");
        save(&path, "printer", [0.1, 0.2]).unwrap();
        let other = path.clone();
        std::thread::spawn(move || save(&other, "clock", [0.3, 0.4]).unwrap())
            .join()
            .unwrap();
        assert_eq!(names(&path).unwrap(), vec!["clock", "printer"]);
        assert_eq!(delete(&path, "printer").unwrap()["deleted"], true);
        assert!(get(&path, "printer").is_err());
        assert_eq!(delete(&path, "missing").unwrap()["deleted"], false);
        let writers = (0..8)
            .map(|i| {
                let path = path.clone();
                std::thread::spawn(move || save(&path, &format!("view{i}"), [0., 0.]).unwrap())
            })
            .collect::<Vec<_>>();
        for writer in writers {
            writer.join().unwrap();
        }
        assert_eq!(clear(&path).unwrap()["cleared"], 9);
        save(&path, "new", [0., 0.]).unwrap();
        assert_eq!(names(&path).unwrap(), vec!["new"]);
        fs::write(&path, b"invalid").unwrap();
        assert!(clear(&path).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"invalid");
        fs::remove_dir_all(dir).unwrap();
    }
}
