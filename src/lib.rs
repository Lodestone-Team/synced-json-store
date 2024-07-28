use notify::{Event, RecursiveMode};
use notify_debouncer_full::{
    new_debouncer,
    notify::{ReadDirectoryChangesWatcher, Watcher},
    DebouncedEvent, Debouncer, FileIdMap,
};
use std::error::Error;
use std::ops::Deref;
use std::{
    cell::{RefCell, RefMut},
    ops::DerefMut,
    path::{Path, PathBuf},
    rc::Rc,
    time::Duration,
};
pub struct SyncedJsonStore<T>
where
    T: serde::Serialize + for<'a> serde::Deserialize<'a>,
{
    data: Rc<RefCell<T>>,
    path: PathBuf,
    listener: Option<Debouncer<ReadDirectoryChangesWatcher, FileIdMap>>,
}

fn write_data_to_file<T: serde::Serialize + for<'a> serde::Deserialize<'a>>(
    data: &T,
    path: impl AsRef<Path>,
) -> Result<(), std::io::Error> {
    let path = path.as_ref();
    // open file and write data, create file if it doesn't exist
    let file = std::fs::File::options()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)?;
    serde_json::to_writer(file, data)?;
    Ok(())
}

impl<T> SyncedJsonStore<T>
where
    T: serde::Serialize + for<'a> serde::Deserialize<'a>,
{
    pub fn new(data: T, path: impl AsRef<Path>, overwrite: bool) -> Result<Self, std::io::Error> {
        let path = path.as_ref();
        if overwrite {
            let file = std::fs::File::options()
                .write(true)
                .create(true)
                .truncate(true)
                .open(path)?;
            serde_json::to_writer(file, &data)?;
        } else {
            let file = std::fs::File::create_new(path)?;
            serde_json::to_writer(file, &data)?;
        }
        Ok(Self {
            data: Rc::new(RefCell::new(data)),
            path: path.to_path_buf(),
            listener: None,
        })
    }

    pub fn new_with_listener<F>(
        data: T,
        path: impl AsRef<Path>,
        overwrite: bool,
        mut callback: F,
    ) -> Result<Self, Box<dyn Error>>
    where
        F: FnMut(&Event) + Send + 'static,
    {
        let mut self_ref = Self::new(data, path.as_ref(), overwrite)?;

        // init listener
        let mut listener: Debouncer<ReadDirectoryChangesWatcher, FileIdMap> = new_debouncer(
            Duration::from_millis(500),
            None,
            move |result: Result<Vec<DebouncedEvent>, Vec<notify::Error>>| {
                let events: Vec<DebouncedEvent> = result.unwrap_or_default();
                events.iter().for_each(|event| {
                    callback(&event.event);
                });
            },
        )?;

        listener
            .watcher()
            .watch(path.as_ref(), RecursiveMode::NonRecursive)?;
        listener
            .cache()
            .add_root(path.as_ref(), RecursiveMode::NonRecursive);
        self_ref.listener = Some(listener);
        Ok(self_ref)
    }

    pub fn from_file(path: impl AsRef<Path>) -> Result<Self, std::io::Error> {
        let path = path.as_ref();
        // open file and read data
        let file = std::fs::File::open(path)?;
        let data = serde_json::from_reader(file)?;
        Ok(Self {
            data: Rc::new(RefCell::new(data)),
            path: path.to_path_buf(),
            listener: None,
        })
    }

    pub fn replace(&mut self, new_data: T) -> Result<(), std::io::Error> {
        // try to write new data to file, if it fails, keep the old data
        write_data_to_file(&new_data, &self.path)?;
        self.data.replace(new_data);
        Ok(())
    }
}

impl<T> SyncedJsonStore<T>
where
    T: serde::Serialize + for<'a> serde::Deserialize<'a> + Clone,
{
    pub fn update_with<F>(&mut self, f: F) -> Result<(), std::io::Error>
    where
        F: FnOnce(&mut T),
    {
        let mut temp_data = self.data.borrow().clone();
        f(&mut temp_data);
        write_data_to_file(&temp_data, &self.path)?;
        self.data.replace(temp_data);
        Ok(())
    }
    pub fn get_data(&self) -> T {
        self.data.borrow().clone()
    }
    #[must_use]
    pub fn get_mut(&self) -> SyncGuard<T> {
        SyncGuard {
            guard: RefCell::borrow_mut(&self.data),
            path: self.path.clone(),
            buf: None,
        }
    }
}

pub struct SyncGuard<'a, T>
where
    T: serde::Serialize + for<'b> serde::Deserialize<'b>,
{
    guard: RefMut<'a, T>,
    buf: Option<T>,
    path: PathBuf,
}

impl<'a, T> Deref for SyncGuard<'a, T>
where
    T: serde::Serialize + for<'b> serde::Deserialize<'b>,
{
    type Target = T;

    fn deref(&self) -> &Self::Target {
        if let Some(buf) = &self.buf {
            buf
        } else {
            &self.guard
        }
    }
}

impl<'a, T> DerefMut for SyncGuard<'a, T>
where
    T: serde::Serialize + for<'b> serde::Deserialize<'b> + ToOwned<Owned = T>,
{
    fn deref_mut(&mut self) -> &mut Self::Target {
        // Clone the data if it hasn't been cloned yet
        if self.buf.is_none() {
            self.buf = Some(self.guard.to_owned());
        }
        self.buf.as_mut().unwrap()
    }
}

impl<T> Drop for SyncGuard<'_, T>
where
    T: serde::Serialize + for<'a> serde::Deserialize<'a>,
{
    fn drop(&mut self) {
        if let Some(buf) = self.buf.take() {
            if write_data_to_file(&buf, &self.path).is_ok() {
                *self.guard = buf;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::fs;
    use std::io::{BufWriter, Write};
    use std::sync::{Arc, Mutex};
    use std::thread::{self};
    #[test]
    fn test_new() {
        #[derive(serde::Serialize, serde::Deserialize, Clone)]
        struct TestStruct {
            data: i32,
            string: String,
        }
        let data = TestStruct {
            data: 42,
            string: "Hello, World!".to_string(),
        };
        let path = "test_new.json";
        let store = SyncedJsonStore::new(data, path, true).unwrap();
        store.get_mut().data = 43;
        let file = fs::File::open(path).unwrap();
        let data: TestStruct = serde_json::from_reader(file).unwrap();
        assert_eq!(data.data, 43);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn test_listen() {
        #[derive(serde::Serialize, serde::Deserialize, Clone)]
        struct TestStruct {
            data: i32,
            string: String,
        }
        let initial_data = TestStruct {
            data: 100,
            string: "Hello!".to_string(),
        };
        let path = "test_listen.json";
        let mut stuff: Arc<Mutex<i32>> = Arc::new(Mutex::new(0));
        let callback = {
            let mut stuff = stuff.clone();
            move |_event: &Event| {
                *stuff.clone().lock().unwrap() = 1;
            }
        };
        let store = SyncedJsonStore::new_with_listener(initial_data, path, true, callback).unwrap();
        store.get_mut().data = 1337;

        // wait for debouncer
        thread::sleep(Duration::from_secs(1));

        assert_eq!(stuff.lock().unwrap().clone(), 1);

        std::fs::remove_file(path).unwrap();
    }
}
