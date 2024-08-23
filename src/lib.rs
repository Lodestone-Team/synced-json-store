use notify::{
    event::{self, ModifyKind},
    Event, EventKind, RecursiveMode,
};
use notify_debouncer_full::{
    new_debouncer,
    notify::{ReadDirectoryChangesWatcher, Watcher},
    DebouncedEvent, Debouncer, FileIdMap,
};
use std::{
    cell::{RefCell, RefMut},
    ops::DerefMut,
    path::{Path, PathBuf},
    rc::Rc,
    time::Duration,
};
use std::{collections::HashMap, ops::Deref};
use std::{
    error::Error,
    fs,
    io::{BufReader, ErrorKind},
    sync::{Arc, Mutex, MutexGuard},
};
pub struct SyncedJsonStore<T>
where
    T: serde::Serialize + for<'a> serde::Deserialize<'a>,
{
    data: Rc<RefCell<T>>,
    path: PathBuf,
    listener: Option<Debouncer<ReadDirectoryChangesWatcher, FileIdMap>>,
    callbacks: Arc<Mutex<HashMap<String, Box<dyn Fn(&Event) + Send>>>>,
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
    pub fn set_listener(&mut self, path: impl AsRef<Path>) -> Result<(), std::io::Error> {
        let path = path.as_ref();
        let callbacks = self.callbacks.clone();
        let mut listener: Debouncer<ReadDirectoryChangesWatcher, FileIdMap> = match new_debouncer(
            Duration::from_millis(500),
            None,
            move |result: Result<Vec<DebouncedEvent>, Vec<notify::Error>>| {
                let events: Vec<DebouncedEvent> = result.unwrap_or_default();
                let callbacks = callbacks.clone();
                let callbacks = callbacks.lock().unwrap();
                for event in events.into_iter() {
                    let event = event.event;
                    for callback in callbacks.values() {
                        callback(&event);
                    }
                }
            },
        ) {
            Ok(res) => res,
            Err(e) => return Err(std::io::Error::new(std::io::ErrorKind::Other, e)),
        };

        match listener
            .watcher()
            .watch(path.as_ref(), RecursiveMode::NonRecursive)
        {
            Ok(_) => {}
            Err(e) => return Err(std::io::Error::new(std::io::ErrorKind::Other, e)),
        };
        listener
            .cache()
            .add_root(path.to_path_buf(), RecursiveMode::NonRecursive);
        self.listener = Some(listener);
        Ok(())
    }

    fn set_callback(
        &mut self,
        key: &str,
        callback: impl Fn(&Event) + Send + 'static,
    ) -> Result<(), String> {
        let callbacks = self.callbacks.clone();
        // TODO - fix error handling here
        let mut callbacks = match callbacks.lock() {
            Ok(res) => res,
            Err(e) => return Err(e.to_string()),
        };
        callbacks.insert(key.to_string(), Box::new(callback));

        Ok(())
    }
    fn remove_callback(&mut self, key: &str) -> Result<(), String> {
        let callbacks = self.callbacks.clone();
        // TODO - fix error handling here
        let mut callbacks = match callbacks.lock() {
            Ok(res) => res,
            Err(e) => return Err(e.to_string()),
        };
        callbacks.remove(key);
        Ok(())
    }

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
            callbacks: Arc::new(Mutex::new(HashMap::new())),
        })
    }
    pub fn new_with_listener(
        data: T,
        path: impl AsRef<Path>,
        overwrite: bool,
    ) -> Result<Self, std::io::Error> {
        let mut curr = Self::new(data, path.as_ref(), overwrite)?;
        curr.set_listener(path.as_ref());
        Ok(curr)
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
            callbacks: Arc::new(Mutex::new(HashMap::new())),
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

pub struct ArcSyncedJsonStore<T>
where
    T: serde::Serialize + for<'a> serde::Deserialize<'a>,
{
    data: Arc<Mutex<T>>,
    path: PathBuf,
    listener: Option<Debouncer<ReadDirectoryChangesWatcher, FileIdMap>>,
    callbacks: Arc<Mutex<HashMap<String, Box<dyn Fn(&Event) + Send>>>>,
}

impl<T> ArcSyncedJsonStore<T>
where
    T: serde::Serialize + for<'a> serde::Deserialize<'a> + Send + 'static,
{
    pub fn set_callback(
        &mut self,
        key: &str,
        callback: impl Fn(&Event) + Send + 'static,
    ) -> Result<(), String> {
        let callbacks = self.callbacks.clone();
        // TODO - fix error handling here
        let mut callbacks = match callbacks.lock() {
            Ok(res) => res,
            Err(e) => return Err(e.to_string()),
        };
        callbacks.insert(key.to_string(), Box::new(callback));

        Ok(())
    }
    pub fn remove_callback(&mut self, key: &str) -> Result<(), String> {
        let callbacks = self.callbacks.clone();
        // TODO - fix error handling here
        let mut callbacks = match callbacks.lock() {
            Ok(res) => res,
            Err(e) => return Err(e.to_string()),
        };
        callbacks.remove(key);
        Ok(())
    }
    pub fn add_listener(
        &mut self,
        data: Arc<Mutex<T>>,
        path: impl AsRef<Path>,
    ) -> Result<(), std::io::Error> {
        let path = path.as_ref();
        let callbacks = self.callbacks.clone();
        let mut listener: Debouncer<ReadDirectoryChangesWatcher, FileIdMap> = match new_debouncer(
            Duration::from_millis(500),
            None,
            move |result: Result<Vec<DebouncedEvent>, Vec<notify::Error>>| {
                let events: Vec<DebouncedEvent> = result.unwrap_or_default();
                let callbacks = callbacks.clone();
                let callbacks = callbacks.lock().unwrap();
                for event in events.into_iter() {
                    let event = event.event;
                    let update_path = match event.paths.get(0) {
                        None => return,
                        Some(res) => res,
                    };
                    let event_kind = match event.kind {
                        EventKind::Modify(kind) => kind,
                        _ => return,
                    };
                    match event_kind {
                        ModifyKind::Any => {}
                        _ => return,
                    };
                    // no need to check filename, assume watching directory to just be the relevant file
                    let file = match fs::File::open(update_path) {
                        Err(_) => return,
                        Ok(res) => res,
                    };
                    let reader = BufReader::new(file);

                    // write to data
                    let data = data.clone();
                    let mut data = match data.lock() {
                        Err(_) => return,
                        Ok(res) => res,
                    };
                    *data = match serde_json::from_reader(reader) {
                        Ok(res) => res,
                        Err(_) => return,
                    };
                    for callback in callbacks.values() {
                        callback(&event);
                    }
                }
            },
        ) {
            Ok(res) => res,
            Err(e) => return Err(std::io::Error::new(std::io::ErrorKind::Other, e)),
        };

        match listener
            .watcher()
            .watch(path.as_ref(), RecursiveMode::NonRecursive)
        {
            Ok(_) => {}
            Err(e) => return Err(std::io::Error::new(std::io::ErrorKind::Other, e)),
        };
        listener
            .cache()
            .add_root(path.to_path_buf(), RecursiveMode::NonRecursive);
        self.listener = Some(listener);
        Ok(())
    }
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

        let data = Arc::new(Mutex::new(data));
        let data_struct = data.clone();

        Ok(Self {
            data: data_struct,
            path: path.to_path_buf(),
            listener: None,
            callbacks: Arc::new(Mutex::new(HashMap::new())),
        })
    }
    pub fn new_with_listener(
        data: T,
        path: impl AsRef<Path>,
        overwrite: bool,
    ) -> Result<Self, std::io::Error> {
        let mut current_instance = Self::new(data, path.as_ref(), overwrite)?;
        current_instance.add_listener(current_instance.data.clone(), path.as_ref())?;
        Ok(current_instance)
    }
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self, std::io::Error> {
        let path = path.as_ref();
        // open file and read data
        let file = std::fs::File::open(path)?;
        let data = serde_json::from_reader(file)?;
        Ok(Self {
            data: Arc::new(Mutex::new(data)),
            path: path.to_path_buf(),
            listener: None,
            callbacks: Arc::new(Mutex::new(HashMap::new())),
        })
    }
    pub fn replace(&mut self, new_data: T) -> Result<(), std::io::Error> {
        // try to write new data to file, if it fails, keep the old data

        write_data_to_file(&new_data, &self.path)?;

        let old_data = self.data.clone();
        let mut old_data = match old_data.lock() {
            Err(e) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    e.to_string(),
                ))
            }
            Ok(res) => res,
        };
        *old_data = new_data;
        Ok(())
    }
}

impl<T> ArcSyncedJsonStore<T>
where
    T: serde::Serialize + for<'a> serde::Deserialize<'a> + Clone,
{
    pub fn update_with<F>(&mut self, f: F) -> Result<(), std::io::Error>
    where
        F: FnOnce(&mut T),
    {
        let temp_data = self.data.clone();
        let mut temp_data = match temp_data.lock() {
            Err(e) => return Err(std::io::Error::new(ErrorKind::InvalidData, e.to_string())),
            Ok(res) => res,
        };

        f(&mut temp_data);
        write_data_to_file(&(*temp_data), &self.path)?;
        Ok(())
    }
    pub fn get_data(&self) -> Arc<Mutex<T>> {
        self.data.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, File};
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
    fn test_sync_callback() {
        #[derive(serde::Serialize, serde::Deserialize, Clone)]
        struct TestStruct {
            data: i32,
            string: String,
        }
        let data = TestStruct {
            data: 42,
            string: "Hello, World!".to_string(),
        };
        let path = "test_sync_callback.json";
        let mut store = SyncedJsonStore::new_with_listener(data, path, true).unwrap();

        let shared1 = Arc::new(Mutex::new(0));
        let shared1_curr = shared1.clone();
        store
            .set_callback("setValue", move |_| {
                let shared1 = shared1.clone();
                let mut shared1 = shared1.lock().unwrap();
                *shared1 = 1;
            })
            .unwrap();

        let new_data = TestStruct {
            data: 139,
            string: "New string".to_string(),
        };
        write_data_to_file(&new_data, &path).unwrap();
        thread::sleep(Duration::from_secs(1));
        assert_eq!(store.get_data().data, 42);
        let shared1_curr = shared1_curr.lock().unwrap();
        assert_eq!(*shared1_curr, 1);

        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn test_new_arc() {
        #[derive(serde::Serialize, serde::Deserialize, Clone)]
        struct TestStruct {
            data: i32,
            string: String,
        }
        let data = TestStruct {
            data: 42,
            string: "Hello, World!".to_string(),
        };
        let path = "test_new_arc.json";

        thread::spawn(move || {
            let mut store = ArcSyncedJsonStore::new_with_listener(data, path, true).unwrap();
            let _ = store.update_with(|d: &mut TestStruct| {
                d.data = 4;
            });
        });
        thread::sleep(Duration::from_millis(1000));

        let file = File::open(path).unwrap();
        let data: TestStruct = serde_json::from_reader(&file).unwrap();
        assert_eq!(data.data, 4);

        let store = ArcSyncedJsonStore::new_with_listener(data, path, true).unwrap();
        let data = TestStruct {
            data: 42,
            string: "hi".to_string(),
        };
        let _write_result = write_data_to_file(&data, path).unwrap_or_else(|e| {
            panic!("{e:?}");
        });

        thread::sleep(Duration::from_millis(1000));
        let data_arc = store.get_data();
        let data_arc = data_arc.lock().unwrap();
        assert_eq!(data_arc.data, 42);

        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn test_arc_callback() {
        #[derive(serde::Serialize, serde::Deserialize, Clone)]
        struct TestStruct {
            data: i32,
            string: String,
        }
        let data = TestStruct {
            data: 42,
            string: "Hello, World!".to_string(),
        };
        let path = "test_arc_callback.json";
        let mut store = ArcSyncedJsonStore::new_with_listener(data, path, true).unwrap();
        let shared1 = Arc::new(Mutex::new(0));
        let shared1_clone = shared1.clone();

        let _ = store.set_callback("valueSetter1", move |_| {
            let shared1 = shared1.clone();
            let mut shared1 = shared1.lock().unwrap();
            *shared1 += 1;
        });

        let data = TestStruct {
            data: 45,
            string: "Hello, World!".to_string(),
        };
        write_data_to_file(&data, path).unwrap();
        thread::sleep(Duration::from_secs(1));
        let shared1_clone = shared1_clone.clone();
        let shared1_clone = shared1_clone.lock().unwrap();
        assert_eq!(*shared1_clone, 1);

        let curr_data = store.data.clone();
        let curr_data = curr_data.lock().unwrap();
        assert_eq!(curr_data.data, 45);

        std::fs::remove_file(path).unwrap();
    }
}
