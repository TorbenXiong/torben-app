import { Button } from "@torben-app/ui";
import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Link, Navigate, Route, Routes, useParams } from "react-router";
import {
  applyManagedUpdate,
  checkManagedUpdates,
  checkTorbenUpdate,
  formatTorbenError,
  getOperationEvents,
  getSnapshot,
  initialTorbenUpdateStatus,
  installBundledMysqlPlugin,
  installBundledNodePlugin,
  installBundledPostgresqlPlugin,
  installBundledPythonPlugin,
  installBundledRedisPlugin,
  installBundledRustPlugin,
  installBundledTemurinPlugin,
  installTorbenUpdate,
  migrateManagedLibrary,
  setShellIntegration,
  uninstallBundledMysqlPlugin,
  uninstallBundledNodePlugin,
  uninstallBundledPostgresqlPlugin,
  uninstallBundledPythonPlugin,
  uninstallBundledRedisPlugin,
  uninstallBundledRustPlugin,
  uninstallBundledTemurinPlugin,
  updateSettings,
} from "./api";
import { Layout } from "./components/Layout";
import i18n from "./i18n";
import { NodeDetailPage } from "./NodeDetailPage";
import {
  DiagnosticsPage,
  LogsPage,
  MysqlDetailPage,
  PluginDetailPage,
  PluginsPage,
  PostgresqlDetailPage,
  PythonDetailPage,
  RedisDetailPage,
  RustDetailPage,
  SettingsPage,
  TemurinDetailPage,
} from "./pages";
import { applyThemePreference, resolveLanguagePreference } from "./preferences";
import type {
  DashboardSnapshot,
  ManagedUpdateCheck,
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
    // Fetch immediately so a just-started install/uninstall remains visible
    // when the user navigates to another plugin before the first interval.
    void poll();
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

  const temurinEnabled = snapshot.plugins.some(
    (plugin) => plugin.id === "app.torben.plugin.temurin" && plugin.enabled,
  );
  const nodeEnabled = snapshot.plugins.some(
    (plugin) => plugin.id === "app.torben.plugin.node" && plugin.enabled,
  );
  const pythonEnabled = snapshot.plugins.some(
    (plugin) => plugin.id === "app.torben.plugin.python" && plugin.enabled,
  );
  const rustEnabled = snapshot.plugins.some(
    (plugin) => plugin.id === "app.torben.plugin.rust" && plugin.enabled,
  );
  const mysqlEnabled = snapshot.plugins.some(
    (plugin) => plugin.id === "app.torben.plugin.mysql" && plugin.enabled,
  );
  const redisEnabled = snapshot.plugins.some(
    (plugin) => plugin.id === "app.torben.plugin.redis" && plugin.enabled,
  );
  const postgresqlEnabled = snapshot.plugins.some(
    (plugin) => plugin.id === "app.torben.plugin.postgresql" && plugin.enabled,
  );

  return (
    <Layout
      applications={snapshot.applications}
      pluginOrder={snapshot.settings.pluginOrder}
      plugins={snapshot.plugins}
      onPluginOrderChange={(pluginOrder) => saveSettings({ ...snapshot.settings, pluginOrder })}
    >
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
          <Link
            to={
              managedUpdates.candidates[0]?.appId === "node"
                ? "/node"
                : managedUpdates.candidates[0]?.appId === "python"
                  ? "/python"
                  : managedUpdates.candidates[0]?.appId === "rust"
                    ? "/rust"
                    : managedUpdates.candidates[0]?.appId === "mysql"
                      ? "/mysql"
                      : managedUpdates.candidates[0]?.appId === "redis"
                        ? "/redis"
                        : managedUpdates.candidates[0]?.appId === "postgresql"
                          ? "/postgresql"
                          : "/java"
            }
          >
            {t("appShell.reviewUpdates")}
          </Link>
        </div>
      ) : null}
      <Routes>
        <Route
          path="/node"
          element={
            nodeEnabled ? (
              <NodeDetailPage
                installed={snapshot.installed}
                operations={snapshot.operations}
                selected={snapshot.selected}
                onChanged={refresh}
                shellIntegration={snapshot.shellIntegration}
              />
            ) : (
              <Navigate replace to="/plugins" />
            )
          }
        />
        <Route
          path="/python"
          element={
            pythonEnabled ? (
              <PythonDetailPage
                installed={snapshot.installed}
                operations={snapshot.operations}
                onChanged={refresh}
                selected={snapshot.selected}
                shellIntegration={snapshot.shellIntegration}
              />
            ) : (
              <Navigate replace to="/plugins" />
            )
          }
        />
        <Route
          path="/rust"
          element={
            rustEnabled ? (
              <RustDetailPage
                installed={snapshot.installed}
                operations={snapshot.operations}
                onChanged={refresh}
                selected={snapshot.selected}
                shellIntegration={snapshot.shellIntegration}
              />
            ) : (
              <Navigate replace to="/plugins" />
            )
          }
        />
        <Route
          path="/mysql"
          element={
            mysqlEnabled ? (
              <MysqlDetailPage
                installed={snapshot.installed}
                operations={snapshot.operations}
                onChanged={refresh}
                selected={snapshot.selected}
                shellIntegration={snapshot.shellIntegration}
              />
            ) : (
              <Navigate replace to="/plugins" />
            )
          }
        />
        <Route
          path="/redis"
          element={
            redisEnabled ? (
              <RedisDetailPage
                installed={snapshot.installed}
                operations={snapshot.operations}
                onChanged={refresh}
                selected={snapshot.selected}
                shellIntegration={snapshot.shellIntegration}
              />
            ) : (
              <Navigate replace to="/plugins" />
            )
          }
        />
        <Route
          path="/postgresql"
          element={
            postgresqlEnabled ? (
              <PostgresqlDetailPage
                installed={snapshot.installed}
                operations={snapshot.operations}
                onChanged={refresh}
                selected={snapshot.selected}
                shellIntegration={snapshot.shellIntegration}
              />
            ) : (
              <Navigate replace to="/plugins" />
            )
          }
        />
        <Route
          path="/java"
          element={
            temurinEnabled ? (
              <TemurinDetailPage
                installed={snapshot.installed}
                operations={snapshot.operations}
                onChanged={refresh}
                selected={snapshot.selected}
                shellIntegration={snapshot.shellIntegration}
              />
            ) : (
              <Navigate replace to="/plugins" />
            )
          }
        />
        <Route
          path="/logs"
          element={<LogsPage events={snapshot.operations} onChanged={refresh} />}
        />
        <Route
          path="/plugins"
          element={
            <PluginsPage
              onChanged={refresh}
              onInstallBundledTemurin={installBundledTemurinPlugin}
              onInstallBundledNode={installBundledNodePlugin}
              onUninstallBundledNode={uninstallBundledNodePlugin}
              onInstallBundledPython={installBundledPythonPlugin}
              onInstallBundledRust={installBundledRustPlugin}
              onInstallBundledMysql={installBundledMysqlPlugin}
              onInstallBundledRedis={installBundledRedisPlugin}
              onInstallBundledPostgresql={installBundledPostgresqlPlugin}
              onUninstallBundledTemurin={uninstallBundledTemurinPlugin}
              onUninstallBundledPython={uninstallBundledPythonPlugin}
              onUninstallBundledRust={uninstallBundledRustPlugin}
              onUninstallBundledMysql={uninstallBundledMysqlPlugin}
              onUninstallBundledRedis={uninstallBundledRedisPlugin}
              onUninstallBundledPostgresql={uninstallBundledPostgresqlPlugin}
              onPluginOrderChange={(pluginOrder) =>
                saveSettings({ ...snapshot.settings, pluginOrder })
              }
              pluginOrder={snapshot.settings.pluginOrder}
              plugins={snapshot.plugins}
              registry={snapshot.pluginRegistry}
            />
          }
        />
        <Route
          path="/plugins/:pluginId"
          element={
            <PluginDetailRoute
              onSettingsChange={saveSettings}
              plugins={snapshot.plugins}
              settings={snapshot.settings}
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
        <Route path="/overview" element={<Navigate replace to="/plugins" />} />
        <Route path="/catalog/*" element={<Navigate replace to="/plugins" />} />
        <Route path="/installed" element={<Navigate replace to="/plugins" />} />
        <Route path="/tasks" element={<Navigate replace to="/logs" />} />
        <Route path="*" element={<Navigate replace to="/plugins" />} />
      </Routes>
    </Layout>
  );
}

function PluginDetailRoute({
  onSettingsChange,
  plugins,
  settings,
}: {
  onSettingsChange: (settings: UserSettings) => Promise<void>;
  plugins: DashboardSnapshot["plugins"];
  settings: UserSettings;
}) {
  const { pluginId } = useParams();
  let decodedId = pluginId ?? "";
  try {
    decodedId = decodeURIComponent(decodedId);
  } catch {
    // Keep the raw route parameter so the page can render its empty state.
  }
  return (
    <PluginDetailPage
      onSettingsChange={onSettingsChange}
      plugin={plugins.find((plugin) => plugin.id === decodedId) ?? null}
      settings={settings}
    />
  );
}
