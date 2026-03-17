use crate::core::project::Project;
use std::collections::HashMap;
use uuid::Uuid;

pub struct SessionManager {
    pub projects: HashMap<Uuid, Project>,
    pub active_project_id: Option<Uuid>,
}

impl SessionManager {
    pub fn new() -> Self {
        Self {
            projects: HashMap::new(),
            active_project_id: None,
        }
    }

    pub fn create_new_project(&mut self) {
        let project = Project::new();
        self.active_project_id = Some(project.id);
        self.projects.insert(project.id, project);
    }

    pub fn get_active_project(&mut self) -> Option<&mut Project> {
        if let Some(id) = self.active_project_id {
            self.projects.get_mut(&id)
        } else {
            None
        }
    }
}
