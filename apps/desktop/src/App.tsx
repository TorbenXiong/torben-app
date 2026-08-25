import { Button } from "@torben-app/ui";
import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Link, Navigate, Route, Routes } from "react-router";
import {
  applyManagedUpdate,
  checkManagedUpdates,
  checkTorbenUpdate,
  formatTorbenError,
  getOperationEvents,
  getSnapshot,
  initialTorbenUpdateStatus,
  installTorbenUpdate,
  migrateManagedLibrary,
  setManagedAutoUpdate,
  setShellIntegration,
  updateSettings,
} from "./api";
import { Layout } from "./components/Layout";
import i18n from "./i18n";
import {
  CatalogPage,
  CodexDetailPage,
  DiagnosticsPage,
  GitDetailPage,
  InstalledPage,
  NodeDetailPage,
  OverviewPage,
  PluginsPage,
  PythonDetailPage,
  SettingsPage,
  TasksPage,
  TemurinDetailPage,
  VsCodeDetailPage,
} from "./pages";
import { applyThemePreference, resolveLanguagePreference } from "./preferences";
import type {
  DashboardSnapshot,
  ManagedUpdateCandidate,
  ManagedUpdateCheck,
  ManagedUpdateResult,
  TorbenUpdateStatus,
  UserSettings,
} from "./types";

export default function App() {
  const { t } = useTranslation();
  const [snapshot, setSnapshot] = useState<DashboardSnapshot | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [operationPollingError, setOperationPollingError] = useState<string | null>(null);
  const [updateStatus, setUpdateStatus] = useState<TorbenUpdateStatus | null>(null);
  const [managedUpdates, setManagedUpdates] = useState<ManagedUpdateCheck>({
    checkedApps: 0,
    candidates: [],
    warnings: [],
  });
  const automaticUpdateCheckStarted = useRef(false);
  const managedUpdateStartupStarted = useRef(false);

  const refresh = useCallback(async () => {
    try {
      setSnapshot(await getSnapshot());
      setError(null);
    } catch (reason) {
      setError(formatTorbenError(reason));
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  useEffect(() => {
    let stopped = false;
    let timer = 0;
    const poll = async () => {
      try {
        const operations = await getOperationEvents();
        if (!stopped) {
          setSnapshot((current) => (current ? { ...current, operations } : current));
          setOperationPollingError(null);
        }
      } catch (reason) {
        if (!stopped) {
          setOperationPollingError(formatTorbenError(reason));
        }
      } finally {
        if (!stopped) {
          timer = window.setTimeout(() => void poll(), 1000);
        }
      }
    };
    timer = window.setTimeout(() => void poll(), 1000);
    return () => {
      stopped = true;
      window.clearTimeout(timer);
    };
  }, []);

  const theme = snapshot?.settings.theme;
  const language = snapshot?.settings.language;
  useEffect(() => {
    if (!theme || !language) {
      return undefined;
    }
    const stopWatchingTheme = applyThemePreference(theme);
    void i18n.changeLanguage(resolveLanguagePreference(language));
    return stopWatchingTheme;
  }, [language, theme]);

  const saveSettings = useCallback(async (settings: UserSettings) => {
    await updateSettings(settings);
    setSnapshot((current) => (current ? { ...current, settings } : current));
    setError(null);
  }, []);

  const saveShellIntegration = useCallback(async (enabled: boolean) => {
    const shellIntegration = await setShellIntegration(enabled);
    setSnapshot((current) => (current ? { ...current, shellIntegration } : current));
    setError(null);
  }, []);

  const migrateLibrary = useCallback(
    async (targetPath: string) => {
      const result = await migrateManagedLibrary(targetPath);
      await refresh();
      return result;
    },
    [refresh],
  );

  const refreshManagedUpdates = useCallback(async () => {
    const check = await checkManagedUpdates();
    setManagedUpdates(check);
    return check;
  }, []);

  const applyOneManagedUpdate = useCallback(
    (candidate: ManagedUpdateCandidate): Promise<ManagedUpdateResult> =>
      applyManagedUpdate(candidate),
    [],
  );

  const changeManagedAutoUpdate = useCallback(async (appId: string, enabled: boolean) => {
    const settings = await setManagedAutoUpdate(appId, enabled);
    setSnapshot((current) => (current ? { ...current, settings } : current));
    return settings;
  }, []);

  const checkForTorbenUpdate = useCallback(async () => {
    if (!snapshot) {
      return;
    }
    setUpdateStatus((current) => ({
      ...(current ?? initialTorbenUpdateStatus(snapshot.updater)),
      state: "checking",
      progress: null,
      message: null,
    }));
    try {
      setUpdateStatus(await checkTorbenUpdate(snapshot.updater));
    } catch (reason) {
      setUpdateStatus({
        ...initialTorbenUpdateStatus(snapshot.updater),
        state: "error",
        message: formatTorbenError(reason),
      });
    }
  }, [snapshot]);

  const installAvailableTorbenUpdate = useCallback(async () => {
    if (!snapshot || !updateStatus?.availableVersion) {
      return;
    }
    setUpdateStatus((current) =>
      current ? { ...current, state: "installing", progress: 0, message: null } : current,
    );
    try {
      await installTorbenUpdate(snapshot.updater, (progress) => {
        setUpdateStatus((current) => (current ? { ...current, progress } : current));
      });
    } catch (reason) {
      setUpdateStatus((current) =>
        current
          ? { ...current, state: "error", progress: null, message: formatTorbenError(reason) }
          : current,
      );
    }
  }, [snapshot, updateStatus?.availableVersion]);

  useEffect(() => {
    if (!snapshot) {
      return;
    }
    setUpdateStatus((current) => current ?? initialTorbenUpdateStatus(snapshot.updater));
    if (
      !automaticUpdateCheckStarted.current &&
      snapshot.updater.configured &&
      snapshot.settings.updates.notifyTorbenApp
    ) {
      automaticUpdateCheckStarted.current = true;
      void checkForTorbenUpdate();
    }
  }, [checkForTorbenUpdate, snapshot]);

  useEffect(() => {
    if (
      !snapshot ||
      managedUpdateStartupStarted.current ||
      (!snapshot.settings.updates.notifyManagedApps &&
        snapshot.settings.updates.automaticallyUpdateApps.length === 0) ||
      !snapshot.installed.some((record) => record.scope === "managed")
    ) {
      return;
    }
    managedUpdateStartupStarted.current = true;
    void refreshManagedUpdates()
      .then(async (check) => {
        const automatic = check.candidates.filter((candidate) => candidate.automatic);
        const failures = [];
        let applied = false;
        for (const candidate of automatic) {
          try {
            await applyManagedUpdate(candidate);
            applied = true;
          } catch (reason) {
            failures.push(`${candidate.appId}: ${formatTorbenError(reason)}`);
          }
        }
        if (applied) {
          await refresh();
        }
        if (automatic.length) {
          await refreshManagedUpdates();
        }
        if (failures.length) {
          throw new Error(failures.join("\n"));
        }
      })
      .catch((reason: unknown) => setError(formatTorbenError(reason)));
  }, [refresh, refreshManagedUpdates, snapshot]);

  if (!snapshot) {
    return (
      <div className="boot-screen">
        <div className="boot-mark">T</div>
        <span role={error ? "alert" : undefined}>{error ?? t("appShell.starting")}</span>
        {error ? <Button onClick={() => void refresh()}>{t("appShell.retry")}</Button> : null}
      </div>
    );
  }

  return (
    <Layout applications={snapshot.applications}>
      {error ? (
        <div className="error-banner" role="alert">
          {error}
        </div>
      ) : null}
      {operationPollingError ? (
        <div className="error-banner" role="alert">
          {operationPollingError}
        </div>
      ) : null}
      {snapshot.warnings.length ? (
        <div className="warning-banner" role="status">
          <strong>
            {t("appShell.externalDiscoveryWarnings", { count: snapshot.warnings.length })}
          </strong>
          <ul>
            {snapshot.warnings.map((warning) => (
              <li key={`${warning.appId}:${warning.code}`}>
                {t("appShell.externalDiscoveryWarning", {
                  appId: warning.appId,
                  message: formatTorbenError(warning),
                })}
              </li>
            ))}
          </ul>
        </div>
      ) : null}
      {managedUpdates.candidates.length ? (
        <div className="notice-banner">
          {t("appShell.updatesAvailable", { count: managedUpdates.candidates.length })}
          <Link to="/installed">{t("appShell.reviewUpdates")}</Link>
        </div>
      ) : null}
      <Routes>
        <Route path="/overview" element={<OverviewPage snapshot={snapshot} />} />
        <Route path="/catalog" element={<CatalogPage applications={snapshot.applications} />} />
        <Route
          path="/catalog/node"
          element={<NodeDetailPage installed={snapshot.installed} onChanged={refresh} />}
        />
        <Route
          path="/catalog/temurin"
          element={<TemurinDetailPage installed={snapshot.installed} onChanged={refresh} />}
        />
        <Route
          path="/catalog/python"
          element={<PythonDetailPage installed={snapshot.installed} onChanged={refresh} />}
        />
        <Route
          path="/catalog/git"
          element={<GitDetailPage installed={snapshot.installed} onChanged={refresh} />}
        />
        <Route
          path="/catalog/vscode"
          element={<VsCodeDetailPage installed={snapshot.installed} onChanged={refresh} />}
        />
        <Route
          path="/catalog/codex"
          element={<CodexDetailPage installed={snapshot.installed} onChanged={refresh} />}
        />
        <Route
          path="/installed"
          element={
            <InstalledPage
              external={snapshot.external}
              records={snapshot.installed}
              selected={snapshot.selected}
              onChanged={refresh}
              onApplyUpdate={applyOneManagedUpdate}
              onAutoUpdateChange={changeManagedAutoUpdate}
              onCheckUpdates={refreshManagedUpdates}
              onSettingsChanged={(settings) =>
                setSnapshot((current) => (current ? { ...current, settings } : current))
              }
              settings={snapshot.settings}
              updates={managedUpdates}
            />
          }
        />
        <Route
          path="/tasks"
          element={<TasksPage events={snapshot.operations} onChanged={refresh} />}
        />
        <Route
          path="/plugins"
          element={
            <PluginsPage
              onChanged={refresh}
              plugins={snapshot.plugins}
              registry={snapshot.pluginRegistry}
            />
          }
        />
        <Route
          path="/diagnostics"
          element={
            <DiagnosticsPage
              applications={snapshot.applications}
              checks={snapshot.doctor}
              installed={snapshot.installed}
              onChanged={refresh}
              packageInstallations={snapshot.packageInstallations}
              sourceAdapters={snapshot.sourceAdapters}
            />
          }
        />
        <Route
          path="/settings"
          element={
            <SettingsPage
              onChange={saveSettings}
              onShellChange={saveShellIntegration}
              managedLibrary={snapshot.managedLibrary}
              onLibraryMigrate={migrateLibrary}
              onInstallTorbenUpdate={installAvailableTorbenUpdate}
              onUpdateCheck={checkForTorbenUpdate}
              settings={snapshot.settings}
              shellIntegration={snapshot.shellIntegration}
              updater={snapshot.updater}
              updateStatus={updateStatus ?? initialTorbenUpdateStatus(snapshot.updater)}
            />
          }
        />
        <Route path="*" element={<Navigate replace to="/overview" />} />
      </Routes>
    </Layout>
  );
}
