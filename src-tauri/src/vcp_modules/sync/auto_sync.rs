//! Coalesced foreground sync. Model recovery always finishes before sync writes.
use super::sync_service::{start_manual_sync, SyncState};
use crate::vcp_modules::db_manager::DbState;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tauri::{AppHandle, Manager, Runtime};
use tokio::sync::{Mutex, Notify};

#[derive(Default)]
pub(crate) struct AutoSync {
    requested: AtomicU64,
    wake: Notify,
    finished: Mutex<BTreeMap<u64, bool>>,
}

pub(crate) fn request<R: Runtime>(app: &AppHandle<R>) {
    if let Some(state) = app.try_state::<AutoSync>() {
        state.requested.fetch_add(1, Ordering::SeqCst);
        state.wake.notify_one();
    }
}

pub(crate) async fn session_finished(app: &AppHandle, id: u64, success: bool) {
    if let Some(state) = app.try_state::<AutoSync>() {
        let mut finished = state.finished.lock().await;
        finished.insert(id, success);
        while finished.len() > 64 {
            finished.pop_first();
        }
        drop(finished);
        state.wake.notify_one();
    }
}

pub(crate) fn install(app: &AppHandle) {
    if !cfg!(target_os = "ios") {
        return;
    }
    if !app.manage(AutoSync::default()) {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let state = app.state::<AutoSync>();
        let mut handled = 0;
        let mut earliest = tokio::time::Instant::now() + Duration::from_secs(5);
        request(&app);
        loop {
            let epoch = state.requested.load(Ordering::SeqCst);
            if epoch == handled
                || !crate::vcp_modules::lifecycle_manager::is_app_in_foreground(&app)
            {
                state.wake.notified().await;
                continue;
            }
            tokio::time::sleep_until(earliest).await;
            // Debounce rapid sends and native foreground callbacks.
            tokio::time::sleep(Duration::from_secs(3)).await;
            if !crate::vcp_modules::lifecycle_manager::is_app_in_foreground(&app) {
                continue;
            }
            let db = app.state::<DbState>();
            let busy =
                sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM active_generations)")
                    .fetch_one(&db.pool)
                    .await
                    .unwrap_or(true);
            if busy {
                tokio::select! { _ = state.wake.notified() => {}, _ = tokio::time::sleep(Duration::from_secs(5)) => {} }
                continue;
            }
            #[cfg(not(target_os = "android"))]
            if let Err(error) = crate::vcp_modules::vcp_client::cleanup_mobile_jobs(&app).await {
                log::warn!("[AutoSync] Remote task cleanup deferred: {error}");
            }
            if !crate::vcp_modules::lifecycle_manager::is_app_in_foreground(&app) {
                continue;
            }
            let epoch = state.requested.load(Ordering::SeqCst);
            earliest = tokio::time::Instant::now() + Duration::from_secs(60);
            match start_manual_sync(app.clone(), app.state::<SyncState>()).await {
                Ok(id) => loop {
                    if let Some(success) = state.finished.lock().await.remove(&id) {
                        if success {
                            handled = epoch;
                        }
                        break;
                    }
                    tokio::select! { _ = state.wake.notified() => {}, _ = tokio::time::sleep(Duration::from_secs(2)) => {} }
                },
                Err(error) => log::debug!("[AutoSync] Deferred: {error}"),
            }
        }
    });
}
