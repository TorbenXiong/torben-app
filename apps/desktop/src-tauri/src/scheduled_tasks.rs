use std::{
    collections::BTreeSet,
    sync::{Arc, LazyLock, Mutex},
    time::Duration,
};

use tauri::{AppHandle, Emitter, Manager, Runtime};
use torben_contracts::AppId;
use torben_core::{TorbenCore, VERSION_CATALOG_REFRESH_INTERVAL};

const STARTUP_GRACE_PERIOD: Duration = Duration::from_mins(2);
const POLL_INTERVAL: Duration = Duration::from_secs(30);
const VERSION_CATALOG_UPDATED_EVENT: &str = "version-catalog-updated";

struct ScheduledTaskDefinition {
    id: &'static str,
    app_id: &'static str,
    interval: Duration,
}

const TASKS: &[ScheduledTaskDefinition] = &[
    ScheduledTaskDefinition {
        id: "refresh-temurin-version-catalog",
        app_id: "temurin",
        interval: VERSION_CATALOG_REFRESH_INTERVAL,
    },
    ScheduledTaskDefinition {
        id: "refresh-python-version-catalog",
        app_id: "python",
        interval: VERSION_CATALOG_REFRESH_INTERVAL,
    },
];

static RUNNING_TASKS: LazyLock<Mutex<BTreeSet<String>>> =
    LazyLock::new(|| Mutex::new(BTreeSet::new()));

pub(crate) fn start<R: Runtime>(core: Arc<TorbenCore>, app: AppHandle<R>) {
    if let Err(error) = std::thread::Builder::new()
        .name("torben-scheduled-tasks".to_owned())
        .spawn(move || {
            std::thread::sleep(STARTUP_GRACE_PERIOD);
            loop {
                if application_is_idle(&app) {
                    for task in TASKS {
                        run_if_due(&core, &app, task);
                    }
                }
                std::thread::sleep(POLL_INTERVAL);
            }
        })
    {
        eprintln!("Torben App could not start scheduled tasks: {error}");
    }
}

fn application_is_idle<R: Runtime>(app: &AppHandle<R>) -> bool {
    idle_from_focus_states(
        app.webview_windows()
            .values()
            .map(|window| window.is_focused().ok()),
    )
}

fn idle_from_focus_states(states: impl IntoIterator<Item = Option<bool>>) -> bool {
    !states.into_iter().any(|focused| focused.unwrap_or(true))
}

pub(crate) fn refresh_after_user_action<R: Runtime>(
    core: Arc<TorbenCore>,
    app: AppHandle<R>,
    app_id: AppId,
) {
    let task_id = format!("refresh-{}-version-catalog", app_id.as_str());
    let spawned_task_id = task_id.clone();
    if let Err(error) = std::thread::Builder::new()
        .name(task_id.clone())
        .spawn(move || refresh(&core, &app, &spawned_task_id, &app_id))
    {
        eprintln!("Torben App could not start {task_id}: {error}");
    }
}

fn run_if_due<R: Runtime>(
    core: &Arc<TorbenCore>,
    app: &AppHandle<R>,
    task: &ScheduledTaskDefinition,
) {
    let app_id = match AppId::new(task.app_id) {
        Ok(app_id) => app_id,
        Err(error) => {
            eprintln!("Torben App scheduled task {} is invalid: {error}", task.id);
            return;
        }
    };
    match core.version_catalog_refresh_due(&app_id, task.interval) {
        Ok(true) => refresh(core, app, task.id, &app_id),
        Ok(false) => {}
        Err(error) if error.code == "app_not_supported" => {}
        Err(error) => eprintln!(
            "Torben App scheduled task {} could not inspect its cache [{}]: {}",
            task.id, error.code, error.message
        ),
    }
}

fn refresh<R: Runtime>(core: &Arc<TorbenCore>, app: &AppHandle<R>, task_id: &str, app_id: &AppId) {
    let Some(_guard) = RunningTaskGuard::acquire(task_id) else {
        return;
    };
    match tauri::async_runtime::block_on(core.refresh_version_catalog(app_id)) {
        Ok(_) => {
            if let Err(error) = app.emit(VERSION_CATALOG_UPDATED_EVENT, app_id.to_string()) {
                eprintln!("Torben App scheduled task {task_id} could not notify the UI: {error}");
            }
        }
        Err(error) if error.code == "app_not_supported" => {}
        Err(error) => eprintln!(
            "Torben App scheduled task {task_id} failed [{}]: {}",
            error.code, error.message
        ),
    }
}

struct RunningTaskGuard(String);

impl RunningTaskGuard {
    fn acquire(task_id: &str) -> Option<Self> {
        let mut running = RUNNING_TASKS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !running.insert(task_id.to_owned()) {
            return None;
        }
        Some(Self(task_id.to_owned()))
    }
}

impl Drop for RunningTaskGuard {
    fn drop(&mut self) {
        RUNNING_TASKS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::{STARTUP_GRACE_PERIOD, TASKS, idle_from_focus_states};

    #[test]
    fn temurin_catalog_refreshes_daily() {
        let task = TASKS.iter().find(|task| task.app_id == "temurin").unwrap();
        assert_eq!(task.interval.as_secs(), 86_400);
        assert_eq!(STARTUP_GRACE_PERIOD.as_secs(), 120);
    }

    #[test]
    fn python_catalog_refreshes_daily() {
        let task = TASKS.iter().find(|task| task.app_id == "python").unwrap();

        assert_eq!(task.id, "refresh-python-version-catalog");
        assert_eq!(task.interval.as_secs(), 86_400);
    }

    #[test]
    fn maintenance_runs_only_when_no_window_is_focused() {
        assert!(idle_from_focus_states([Some(false), Some(false)]));
        assert!(!idle_from_focus_states([Some(false), Some(true)]));
        assert!(!idle_from_focus_states([Some(false), None]));
    }
}
