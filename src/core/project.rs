use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq)]
pub enum ProjectStatus {
    Idle,
    Downloading(f32),
    Ready,
    Processing,
    Done(String),
    Error(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectData {
    pub url: String,
    pub start_time: u32,
    pub duration: u32,
    pub source_path: Option<String>,
    pub filename_base: String,
}

#[derive(Clone)]
pub struct Project {
    pub id: Uuid,
    pub data: Arc<Mutex<ProjectData>>,
    pub status: Arc<Mutex<ProjectStatus>>,
}

impl Project {
    pub fn new() -> Self {
        Self {
            id: Uuid::new_v4(),
            data: Arc::new(Mutex::new(ProjectData {
                url: String::new(),
                start_time: 0,
                duration: 30,
                source_path: None,
                filename_base: String::new(),
            })),
            status: Arc::new(Mutex::new(ProjectStatus::Idle)),
        }
    }

    pub fn clone_project_reference(&self) -> Self {
        self.clone()
    }

    pub fn get_snapshot(&self) -> ProjectData {
        self.data.lock().unwrap().clone()
    }
}
