import { open } from "@tauri-apps/plugin-dialog";
import { Badge, Button, Card, EmptyState, PageHeader, ProgressBar } from "@torben-app/ui";
import {
  Activity,
  ArrowDownToLine,
  ArrowRight,
  Check,
  CheckCircle2,
  CircleAlert,
  Clock3,
  Database,
  ExternalLink,
  FolderArchive,
  GripVertical,
  HardDrive,
  Laptop,
  PackageCheck,
  Play,
  Plus,
  RefreshCw,
  RotateCcw,
  Save,
  ShieldCheck,
  Square,
  TerminalSquare,
  Trash2,
  Wrench,
} from "lucide-react";
import { Dialog } from "radix-ui";
import { type ComponentProps, useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Link } from "react-router";
import {
  backupDatabaseInstance,
  cancelOperation,
  clearSelection,
  createDatabaseInstance,
  deleteDatabaseInstance,
  executeManagedToPackageMigration,
  executePackageToManagedMigration,
  executeSourceMigration,
  executeSourceOperation,
  formatTorbenError,
  getPluginSchemaPages,
  getVersions,
  installApp,
  installOfficialPluginFromRegistry,
  installPlugin,
  invokePluginSchemaAction,
  listDatabaseInstances,
  onVersionCatalogUpdated,
  planManagedToPackageMigration,
  planPackageToManagedMigration,
  planSourceMigration,
  planSourceOperation,
  refreshDatabaseInstanceStatus,
  refreshOfficialPluginRegistry,
  restoreDatabaseInstance,
  selectVersion,
  setPluginEnabled,
  setShellIntegration,
  startDatabaseInstance,
  stopDatabaseInstance,
  uninstallApp,
  updateSettings,
} from "./api";
import { ApplicationIcon } from "./components/ApplicationIcon";
import {
  activeRuntimeOperation,
  RuntimeOperationProgress,
} from "./components/RuntimeOperationProgress";
import i18n from "./i18n";
import { comparePluginOrder, movePlugin, normalizePluginOrder } from "./pluginOrder";
import type {
  ApplicationDescriptor,
  DatabaseEngine,
  DatabaseInstance,
  DesktopUpdaterConfiguration,
  DoctorCheck,
  InstallRecord,
  ManagedLibraryMigrationResult,
  ManagedLibraryStatus,
  ManagedToPackageMigrationPlan,
  ManagedToPackageMigrationResult,
  OperationEvent,
  PackageInstallationRecord,
  PackageToManagedMigrationPlan,
  PackageToManagedMigrationRequest,
  PackageToManagedMigrationResult,
  PluginPermissions,
  PluginRegistryStatus,
  PluginSummary,
  SchemaAction,
  SchemaActionResult,
  SchemaPage,
  SchemaSection,
  SelectionRecord,
  ShellIntegrationStatus,
  SourceAction,
  SourceAdapterKind,
  SourceAdapterStatus,
  SourceExecutionRequest,
  SourceExecutionResult,
  SourceMigrationPlan,
  SourceMigrationRequest,
  SourceMigrationResult,
  SourceOperationPlan,
  SourcePackageKind,
  TorbenUpdateStatus,
  UserSettings,
  VersionDescriptor,
} from "./types";

const bundledApplicationIds: Record<string, string> = {
  "app.torben.plugin.node": "node",
  "app.torben.plugin.temurin": "temurin",
  "app.torben.plugin.python": "python",
  "app.torben.plugin.rust": "rust",
  "app.torben.plugin.mysql": "mysql",
  "app.torben.plugin.redis": "redis",
  "app.torben.plugin.postgresql": "postgresql",
  "app.torben.plugin.git": "git",
  "app.torben.plugin.vscode": "vscode",
  "app.torben.plugin.codex": "codex",
};

const bundledRuntimePages: Record<string, string> = {
  "app.torben.plugin.node": "/node",
  "app.torben.plugin.temurin": "/java",
  "app.torben.plugin.python": "/python",
  "app.torben.plugin.rust": "/rust",
  "app.torben.plugin.mysql": "/mysql",
  "app.torben.plugin.redis": "/redis",
  "app.torben.plugin.postgresql": "/postgresql",
};

const pluginEnvironmentAppIds: Record<string, string> = {
  "app.torben.plugin.node": "node",
  "app.torben.plugin.temurin": "temurin",
  "app.torben.plugin.python": "python",
  "app.torben.plugin.rust": "rust",
};

const environmentVariableSuggestions: Record<string, string[]> = {
  node: ["NODE_OPTIONS", "NODE_EXTRA_CA_CERTS", "NPM_CONFIG_REGISTRY"],
  temurin: ["JAVA_TOOL_OPTIONS", "JDK_JAVA_OPTIONS", "JAVA_HOME"],
  python: ["PYTHONPATH", "PYTHONWARNINGS", "PYTHONUTF8"],
  rust: ["RUSTFLAGS", "RUSTDOCFLAGS", "CARGO_TARGET_DIR"],
};

interface EnvironmentVariableRow {
  id: number;
  name: string;
  value: string;
}

let nextEnvironmentVariableRowId = 1;

function environmentVariableRows(variables: Record<string, string>): EnvironmentVariableRow[] {
  return Object.entries(variables).map(([name, value]) => ({
    id: nextEnvironmentVariableRowId++,
    name,
    value,
  }));
}

export function TemurinDetailPage({
  installed,
  onChanged,
  selected = [],
  operations = [],
  shellIntegration,
}: {
  installed: InstallRecord[];
  onChanged: () => Promise<void>;
  selected?: SelectionRecord[];
  operations?: OperationEvent[];
  shellIntegration?: ShellIntegrationStatus;
}) {
  return (
    <JavaDetailPage
      installed={installed}
      onChanged={onChanged}
      selected={selected}
      operations={operations}
      shellIntegration={shellIntegration}
    />
  );
}

interface PythonVersionRow {
  version: string;
  channel: string;
  available?: VersionDescriptor;
  installed?: InstallRecord;
  selected: boolean;
}

function pythonVersionNumbers(version: string): number[] {
  return version.split(".").map(Number);
}

function comparePythonVersions(left: string, right: string): number {
  const leftParts = pythonVersionNumbers(left);
  const rightParts = pythonVersionNumbers(right);
  for (let index = 0; index < Math.max(leftParts.length, rightParts.length); index += 1) {
    const difference = (leftParts[index] ?? 0) - (rightParts[index] ?? 0);
    if (difference !== 0) return difference;
  }
  return 0;
}

function pythonChannel(version: string): string | undefined {
  const match = version.match(/^(\d+\.\d+)\./);
  return match?.[1];
}

function buildRuntimeVersionRows(
  versions: VersionDescriptor[],
  installed: InstallRecord[],
  selected: SelectionRecord[],
  appId: string,
): PythonVersionRow[] {
  const rows = new Map<string, PythonVersionRow>();
  for (const available of versions) {
    const channel = pythonChannel(available.version);
    if (!channel) continue;
    rows.set(available.version, {
      version: available.version,
      channel,
      available,
      selected: false,
    });
  }
  for (const record of installed.filter((record) => record.appId === appId)) {
    const channel = pythonChannel(record.version);
    if (!channel) continue;
    const current = rows.get(record.version);
    rows.set(record.version, {
      version: record.version,
      channel,
      available: current?.available,
      installed: record,
      selected: false,
    });
  }
  const selectedVersion = selected.find((record) => record.appId === appId)?.version;
  return [...rows.values()]
    .map((row) => ({ ...row, selected: row.version === selectedVersion }))
    .sort((left, right) => comparePythonVersions(right.version, left.version));
}

export function RuntimeDetailPage({
  installed,
  onChanged,
  selected = [],
  operations = [],
  shellIntegration,
  appId,
  displayName,
}: {
  installed: InstallRecord[];
  onChanged: () => Promise<void>;
  selected?: SelectionRecord[];
  operations?: OperationEvent[];
  shellIntegration?: ShellIntegrationStatus;
  appId?: "python" | "rust" | "mysql" | "redis" | "postgresql";
  displayName?: string;
}) {
  const { t } = useTranslation();
  const runtimeAppId = appId ?? "python";
  const runtimeDisplayName = displayName ?? "Python";
  const [versions, setVersions] = useState<VersionDescriptor[]>([]);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState<Set<string>>(() => new Set());
  const [error, setError] = useState<string | null>(null);
  const [activeDatabaseTab, setActiveDatabaseTab] = useState<"versions" | "instances">("versions");

  async function selectPrimary(version: string) {
    await selectVersion(runtimeAppId, version);
    if (
      shellIntegration &&
      (shellIntegration.state === "disabled" || shellIntegration.state === "outdated")
    ) {
      await setShellIntegration(true);
    }
  }

  const readVersions = useCallback(
    async (showLoading: boolean) => {
      if (showLoading) setLoading(true);
      setError(null);
      try {
        setVersions(await getVersions(runtimeAppId));
      } catch (reason) {
        setError(formatTorbenError(reason));
      } finally {
        setLoading(false);
      }
    },
    [runtimeAppId],
  );

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void readVersions(true);
    void onVersionCatalogUpdated((updatedAppId) => {
      if (updatedAppId === runtimeAppId) void readVersions(false);
    })
      .then((stopListening) => {
        if (disposed) stopListening();
        else unlisten = stopListening;
      })
      .catch(() => undefined);
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [readVersions, runtimeAppId]);

  async function run(action: string, operation: () => Promise<unknown>) {
    setBusy((current) => new Set(current).add(action));
    setError(null);
    try {
      await operation();
      await onChanged();
    } catch (reason) {
      setError(formatTorbenError(reason));
      await onChanged().catch(() => undefined);
    } finally {
      setBusy((current) => {
        const next = new Set(current);
        next.delete(action);
        return next;
      });
    }
  }

  const rows = buildRuntimeVersionRows(versions, installed, selected, runtimeAppId);
  const selectedVersion = selected.find((record) => record.appId === runtimeAppId)?.version;
  const databaseEngine = isDatabaseEngine(runtimeAppId) ? runtimeAppId : null;
  return (
    <div className="page-stack">
      {error ? (
        <div className="error-banner" role="alert">
          <CircleAlert size={16} /> {error}
        </div>
      ) : null}
      {databaseEngine ? (
        <div className="database-management-tabs" role="tablist" aria-label={runtimeDisplayName}>
          <button
            aria-controls={`${databaseEngine}-versions-panel`}
            aria-selected={activeDatabaseTab === "versions"}
            className={activeDatabaseTab === "versions" ? "active" : undefined}
            id={`${databaseEngine}-versions-tab`}
            onClick={() => setActiveDatabaseTab("versions")}
            role="tab"
            type="button"
          >
            {t("runtimePage.versionManagement")}
          </button>
          <button
            aria-controls={`${databaseEngine}-instances-panel`}
            aria-selected={activeDatabaseTab === "instances"}
            className={activeDatabaseTab === "instances" ? "active" : undefined}
            id={`${databaseEngine}-instances-tab`}
            onClick={() => setActiveDatabaseTab("instances")}
            role="tab"
            type="button"
          >
            {t("runtimePage.instanceManagement")}
          </button>
        </div>
      ) : null}
      {(!databaseEngine || activeDatabaseTab === "versions") && (
        <div
          className="detail-grid"
          id={databaseEngine ? `${databaseEngine}-versions-panel` : undefined}
          role={databaseEngine ? "tabpanel" : undefined}
        >
          <Card className="version-panel">
            <div className="section-heading">
              <div>
                <span className="eyebrow">{t("runtimePage.officialReleases")}</span>
                <h2>{t("runtimePage.availableVersions")}</h2>
              </div>
              {selectedVersion ? (
                <Button
                  disabled={busy.has("clear-selection")}
                  onClick={() => void run("clear-selection", () => clearSelection(runtimeAppId))}
                  size="sm"
                  variant="secondary"
                >
                  {t("runtimePage.clearSelection")}
                </Button>
              ) : null}
            </div>
            {loading ? (
              <div className="skeleton-list">
                <span />
                <span />
              </div>
            ) : (
              <div className="version-list">
                {rows.length === 0 ? (
                  <p className="version-catalog-status">{t("runtimePage.catalogUpdating")}</p>
                ) : null}
                {rows.map((row) => {
                  const installAction = `install:${row.version}`;
                  const operationEvent = activeRuntimeOperation(
                    operations,
                    runtimeAppId,
                    row.version,
                  );
                  const installEvent =
                    operationEvent?.kind === "install" ? operationEvent : undefined;
                  const uninstallEvent =
                    operationEvent?.kind === "uninstall" ? operationEvent : undefined;
                  const installing = busy.has(installAction) || Boolean(installEvent);
                  const uninstalling =
                    busy.has(`uninstall:${row.version}`) || Boolean(uninstallEvent);
                  return (
                    <div className="version-row runtime-version-row" key={row.version}>
                      <div className="version-main">
                        <strong>
                          {runtimeDisplayName} {row.channel}
                        </strong>
                        <span className="runtime-version-summary">v{row.version}</span>
                      </div>
                      <span className="release-date">
                        {row.available?.releasedAt.slice(0, 10) ?? ""}
                      </span>
                      <span className="version-actions">
                        {row.installed ? (
                          <Badge tone="positive">
                            <Check size={12} /> {t("runtimePage.installed")}
                          </Badge>
                        ) : row.available ? (
                          <Button
                            disabled={installing}
                            onClick={() =>
                              void run(installAction, () =>
                                installApp(runtimeAppId, row.available?.version ?? ""),
                              )
                            }
                            size="sm"
                            variant="secondary"
                          >
                            {installing ? (
                              <RefreshCw className="spin" size={14} />
                            ) : (
                              <ArrowDownToLine size={14} />
                            )}
                            {installing ? t("common.installing") : t("common.install")}
                          </Button>
                        ) : null}
                        {row.installed ? (
                          row.selected ? (
                            <Badge tone="accent">{t("runtimePage.selected")}</Badge>
                          ) : (
                            <Button
                              disabled={busy.has(`select:${row.installed?.version}`)}
                              onClick={() =>
                                void run(`select:${row.installed?.version}`, () =>
                                  selectPrimary(row.installed?.version ?? ""),
                                )
                              }
                              size="sm"
                            >
                              {busy.has(`select:${row.installed.version}`)
                                ? t("runtimePage.selecting")
                                : t("runtimePage.select")}
                            </Button>
                          )
                        ) : null}
                        {row.installed ? (
                          <Dialog.Root>
                            <Dialog.Trigger asChild>
                              <Button disabled={uninstalling} size="sm" variant="danger">
                                {uninstalling ? (
                                  <RefreshCw className="spin" size={14} />
                                ) : (
                                  <Trash2 size={14} />
                                )}{" "}
                                {uninstalling
                                  ? t("runtimePage.uninstalling")
                                  : t("runtimePage.uninstall")}
                              </Button>
                            </Dialog.Trigger>
                            <Dialog.Portal>
                              <Dialog.Overlay className="dialog-overlay" />
                              <Dialog.Content className="dialog-content">
                                <Dialog.Title>
                                  {t("runtimePage.uninstallTitle", {
                                    app: runtimeDisplayName,
                                    version: row.installed.version,
                                  })}
                                </Dialog.Title>
                                <Dialog.Description>
                                  {row.selected
                                    ? t("runtimePage.uninstallSelectedDescription")
                                    : t("runtimePage.uninstallDescription")}
                                </Dialog.Description>
                                <div className="dialog-actions">
                                  <Dialog.Close asChild>
                                    <Button variant="ghost">{t("common.cancel")}</Button>
                                  </Dialog.Close>
                                  <Dialog.Close asChild>
                                    <Button
                                      onClick={() =>
                                        void run(
                                          `uninstall:${row.installed?.version}`,
                                          async () => {
                                            if (row.selected) await clearSelection(runtimeAppId);
                                            await uninstallApp(
                                              runtimeAppId,
                                              row.installed?.version ?? "",
                                            );
                                          },
                                        )
                                      }
                                      variant="danger"
                                    >
                                      {t("runtimePage.uninstall")}
                                    </Button>
                                  </Dialog.Close>
                                </div>
                              </Dialog.Content>
                            </Dialog.Portal>
                          </Dialog.Root>
                        ) : null}
                      </span>
                      <RuntimeOperationProgress
                        event={operationEvent}
                        pending={installing || uninstalling}
                        pendingLabel={uninstalling ? t("runtimePage.uninstalling") : undefined}
                        version={row.version}
                      />
                    </div>
                  );
                })}
              </div>
            )}
          </Card>
        </div>
      )}
      {databaseEngine && activeDatabaseTab === "instances" ? (
        <DatabaseInstancesPanel
          engine={databaseEngine}
          installed={installed.filter((record) => record.appId === databaseEngine)}
          selectedVersion={selectedVersion}
        />
      ) : null}
    </div>
  );
}

function isDatabaseEngine(value: string): value is DatabaseEngine {
  return value === "mysql" || value === "redis" || value === "postgresql";
}

const DATABASE_DEFAULT_PORTS: Record<DatabaseEngine, number> = {
  mysql: 3306,
  redis: 6379,
  postgresql: 5432,
};

function DatabaseInstancesPanel({
  engine,
  installed,
  selectedVersion,
}: {
  engine: DatabaseEngine;
  installed: InstallRecord[];
  selectedVersion?: string;
}) {
  const { t } = useTranslation();
  const [instances, setInstances] = useState<DatabaseInstance[]>([]);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState<Set<string>>(() => new Set());
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [name, setName] = useState("local");
  const [port, setPort] = useState(String(DATABASE_DEFAULT_PORTS[engine]));
  const [runtimeVersion, setRuntimeVersion] = useState(
    selectedVersion ?? installed[0]?.version ?? "",
  );

  const refresh = useCallback(async () => {
    setError(null);
    try {
      setInstances(await listDatabaseInstances(engine));
    } catch (reason) {
      setError(formatTorbenError(reason));
    } finally {
      setLoading(false);
    }
  }, [engine]);

  useEffect(() => {
    setLoading(true);
    void refresh();
  }, [refresh]);

  useEffect(() => {
    if (selectedVersion && installed.some((record) => record.version === selectedVersion)) {
      setRuntimeVersion(selectedVersion);
    } else if (!installed.some((record) => record.version === runtimeVersion)) {
      setRuntimeVersion(installed[0]?.version ?? "");
    }
  }, [installed, runtimeVersion, selectedVersion]);

  async function run(action: string, operation: () => Promise<unknown>) {
    setBusy((current) => new Set(current).add(action));
    setError(null);
    setNotice(null);
    try {
      await operation();
      await refresh();
    } catch (reason) {
      setError(formatTorbenError(reason));
    } finally {
      setBusy((current) => {
        const next = new Set(current);
        next.delete(action);
        return next;
      });
    }
  }

  async function restore(instance: DatabaseInstance) {
    const source = await open({
      directory: false,
      multiple: false,
      title: t("databaseInstances.restorePickerTitle"),
    });
    if (typeof source !== "string") return;
    await run(`restore:${instance.name}`, () =>
      restoreDatabaseInstance({ engine, name: instance.name }, source),
    );
  }

  return (
    <Card className="database-instances-panel">
      <div className="section-heading">
        <div>
          <span className="eyebrow">{t("databaseInstances.eyebrow")}</span>
          <h2>{t("databaseInstances.title")}</h2>
          <p>{t("databaseInstances.description")}</p>
        </div>
        <Dialog.Root>
          <Dialog.Trigger asChild>
            <Button disabled={installed.length === 0} size="sm">
              <Plus size={14} /> {t("databaseInstances.create")}
            </Button>
          </Dialog.Trigger>
          <Dialog.Portal>
            <Dialog.Overlay className="dialog-overlay" />
            <Dialog.Content className="dialog-content">
              <Dialog.Title>{t("databaseInstances.createTitle")}</Dialog.Title>
              <Dialog.Description>{t("databaseInstances.createDescription")}</Dialog.Description>
              <label className="dialog-field">
                <span>{t("databaseInstances.name")}</span>
                <input
                  autoComplete="off"
                  onChange={(event) => setName(event.target.value)}
                  pattern="[a-z0-9_-]{1,64}"
                  value={name}
                />
              </label>
              <label className="dialog-field">
                <span>{t("databaseInstances.runtimeVersion")}</span>
                <select
                  onChange={(event) => setRuntimeVersion(event.target.value)}
                  value={runtimeVersion}
                >
                  {installed.map((record) => (
                    <option key={record.version} value={record.version}>
                      {record.version}
                    </option>
                  ))}
                </select>
              </label>
              <label className="dialog-field">
                <span>{t("databaseInstances.port")}</span>
                <input
                  max={65535}
                  min={1}
                  onChange={(event) => setPort(event.target.value)}
                  type="number"
                  value={port}
                />
              </label>
              <div className="dialog-actions">
                <Dialog.Close asChild>
                  <Button variant="ghost">{t("common.cancel")}</Button>
                </Dialog.Close>
                <Dialog.Close asChild>
                  <Button
                    disabled={!name || !runtimeVersion || !Number(port)}
                    onClick={() =>
                      void run("create", () =>
                        createDatabaseInstance({
                          engine,
                          name,
                          runtimeVersion,
                          port: Number(port),
                        }),
                      )
                    }
                  >
                    {t("databaseInstances.create")}
                  </Button>
                </Dialog.Close>
              </div>
            </Dialog.Content>
          </Dialog.Portal>
        </Dialog.Root>
      </div>
      {installed.length === 0 ? (
        <p className="version-catalog-status">{t("databaseInstances.installRuntimeFirst")}</p>
      ) : null}
      {error ? (
        <div className="error-banner" role="alert">
          <CircleAlert size={16} /> {error}
        </div>
      ) : null}
      {notice ? <p className="database-instance-notice">{notice}</p> : null}
      {loading ? (
        <div className="skeleton-list">
          <span />
        </div>
      ) : instances.length === 0 ? (
        <EmptyState
          description={t("databaseInstances.emptyDescription")}
          title={t("databaseInstances.emptyTitle")}
        />
      ) : (
        <div className="database-instance-list">
          {instances.map((instance) => {
            const target = { engine, name: instance.name };
            const actionBusy = [...busy].some((action) => action.endsWith(`:${instance.name}`));
            const restoreReady = instance.state === (engine === "redis" ? "stopped" : "running");
            return (
              <div className="database-instance-row" key={instance.name}>
                <div className="database-instance-main">
                  <strong>{instance.name}</strong>
                  <Badge
                    tone={
                      instance.state === "running"
                        ? "positive"
                        : instance.state === "stale"
                          ? "warning"
                          : "neutral"
                    }
                  >
                    {t(`databaseInstances.state.${instance.state}`)}
                  </Badge>
                  <span>
                    v{instance.runtimeVersion} ·{" "}
                    {t("databaseInstances.portValue", { port: instance.port })}
                  </span>
                </div>
                <div className="database-instance-actions">
                  {instance.state === "stopped" ? (
                    <Button
                      disabled={actionBusy}
                      onClick={() =>
                        void run(`start:${instance.name}`, () => startDatabaseInstance(target))
                      }
                      size="sm"
                    >
                      <Play size={13} /> {t("databaseInstances.start")}
                    </Button>
                  ) : null}
                  {instance.state === "running" ? (
                    <Button
                      disabled={actionBusy}
                      onClick={() =>
                        void run(`stop:${instance.name}`, () => stopDatabaseInstance(target))
                      }
                      size="sm"
                      variant="secondary"
                    >
                      <Square size={13} /> {t("databaseInstances.stop")}
                    </Button>
                  ) : null}
                  <Button
                    aria-label={t("databaseInstances.refreshStatus")}
                    disabled={actionBusy}
                    onClick={() =>
                      void run(`status:${instance.name}`, () =>
                        refreshDatabaseInstanceStatus(target),
                      )
                    }
                    size="sm"
                    variant="ghost"
                  >
                    <RefreshCw size={13} />
                  </Button>
                  <Button
                    disabled={actionBusy || instance.state !== "running"}
                    onClick={() =>
                      void run(`backup:${instance.name}`, async () => {
                        const backup = await backupDatabaseInstance(target);
                        setNotice(t("databaseInstances.backupCreated", { path: backup.path }));
                      })
                    }
                    size="sm"
                    variant="secondary"
                  >
                    <Save size={13} /> {t("databaseInstances.backup")}
                  </Button>
                  <Button
                    disabled={actionBusy || !restoreReady}
                    onClick={() => void restore(instance)}
                    size="sm"
                    variant="secondary"
                  >
                    <RotateCcw size={13} /> {t("databaseInstances.restore")}
                  </Button>
                  <Dialog.Root>
                    <Dialog.Trigger asChild>
                      <Button
                        disabled={actionBusy || instance.state !== "stopped"}
                        size="sm"
                        variant="danger"
                      >
                        <Trash2 size={13} /> {t("databaseInstances.delete")}
                      </Button>
                    </Dialog.Trigger>
                    <Dialog.Portal>
                      <Dialog.Overlay className="dialog-overlay" />
                      <Dialog.Content className="dialog-content">
                        <Dialog.Title>
                          {t("databaseInstances.deleteTitle", { name: instance.name })}
                        </Dialog.Title>
                        <Dialog.Description>
                          {t("databaseInstances.deleteDescription")}
                        </Dialog.Description>
                        <div className="dialog-actions">
                          <Dialog.Close asChild>
                            <Button variant="ghost">{t("common.cancel")}</Button>
                          </Dialog.Close>
                          <Dialog.Close asChild>
                            <Button
                              onClick={() =>
                                void run(`delete:${instance.name}`, () =>
                                  deleteDatabaseInstance(target),
                                )
                              }
                              variant="danger"
                            >
                              {t("databaseInstances.delete")}
                            </Button>
                          </Dialog.Close>
                        </div>
                      </Dialog.Content>
                    </Dialog.Portal>
                  </Dialog.Root>
                </div>
                <code className="database-instance-path">{instance.dataPath}</code>
              </div>
            );
          })}
        </div>
      )}
    </Card>
  );
}

export function PythonDetailPage(
  props: Omit<ComponentProps<typeof RuntimeDetailPage>, "appId" | "displayName">,
) {
  return <RuntimeDetailPage {...props} />;
}

export function RustDetailPage(props: ComponentProps<typeof RuntimeDetailPage>) {
  return <RuntimeDetailPage {...props} appId="rust" displayName="Rust" />;
}

export function MysqlDetailPage(props: ComponentProps<typeof RuntimeDetailPage>) {
  return <RuntimeDetailPage {...props} appId="mysql" displayName="MySQL" />;
}

export function RedisDetailPage(props: ComponentProps<typeof RuntimeDetailPage>) {
  return <RuntimeDetailPage {...props} appId="redis" displayName="Redis" />;
}

export function PostgresqlDetailPage(props: ComponentProps<typeof RuntimeDetailPage>) {
  return <RuntimeDetailPage {...props} appId="postgresql" displayName="PostgreSQL" />;
}

interface JavaVersionRow {
  version: string;
  channel: string;
  available?: VersionDescriptor;
  installed?: InstallRecord;
  selected: boolean;
}

function javaVersionNumbers(version: string): number[] {
  return (version.match(/\d+/g) ?? []).map(Number);
}

function compareJavaVersions(left: string, right: string): number {
  const leftParts = javaVersionNumbers(left);
  const rightParts = javaVersionNumbers(right);
  const length = Math.max(leftParts.length, rightParts.length);
  for (let index = 0; index < length; index += 1) {
    const difference = (leftParts[index] ?? 0) - (rightParts[index] ?? 0);
    if (difference !== 0) return difference;
  }
  return left.localeCompare(right);
}

function javaMajor(version: string): string | undefined {
  return version.match(/^\d+/)?.[0];
}

function buildJavaVersionRows(
  versions: VersionDescriptor[],
  installed: InstallRecord[],
  selected: SelectionRecord[],
): JavaVersionRow[] {
  const rows = new Map<string, JavaVersionRow>();
  for (const available of versions) {
    const channel = javaMajor(available.version);
    if (!channel) continue;
    rows.set(available.version, {
      version: available.version,
      channel,
      available,
      selected: false,
    });
  }
  for (const record of installed.filter((record) => record.appId === "temurin")) {
    const channel = javaMajor(record.version);
    if (!channel) continue;
    const current = rows.get(record.version);
    rows.set(record.version, {
      version: record.version,
      channel,
      available: current?.available,
      installed: record,
      selected: false,
    });
  }
  const selectedVersion = selected.find((record) => record.appId === "temurin")?.version;
  return [...rows.values()]
    .map((row) => ({ ...row, selected: row.version === selectedVersion }))
    .sort((left, right) => compareJavaVersions(right.version, left.version));
}

function JavaDetailPage({
  installed,
  onChanged,
  selected = [],
  operations = [],
  shellIntegration,
}: {
  installed: InstallRecord[];
  onChanged: () => Promise<void>;
  selected?: SelectionRecord[];
  operations?: OperationEvent[];
  shellIntegration?: ShellIntegrationStatus;
}) {
  const { t } = useTranslation();
  const [versions, setVersions] = useState<VersionDescriptor[]>([]);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState<Set<string>>(() => new Set());
  const [error, setError] = useState<string | null>(null);

  async function selectPrimary(version: string) {
    await selectVersion("temurin", version);
    if (
      shellIntegration &&
      (shellIntegration.state === "disabled" || shellIntegration.state === "outdated")
    ) {
      await setShellIntegration(true);
    }
  }

  async function run(action: string, operation: () => Promise<unknown>) {
    setBusy((current) => new Set(current).add(action));
    setError(null);
    try {
      await operation();
      await onChanged();
    } catch (reason) {
      setError(formatTorbenError(reason));
      await onChanged().catch(() => undefined);
    } finally {
      setBusy((current) => {
        const next = new Set(current);
        next.delete(action);
        return next;
      });
    }
  }

  const readVersions = useCallback(async (showLoading: boolean) => {
    if (showLoading) setLoading(true);
    setError(null);
    try {
      setVersions(await getVersions("temurin"));
    } catch (reason) {
      setError(formatTorbenError(reason));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void readVersions(true);
    void onVersionCatalogUpdated((updatedAppId) => {
      if (updatedAppId === "temurin") void readVersions(false);
    })
      .then((stopListening) => {
        if (disposed) stopListening();
        else unlisten = stopListening;
      })
      .catch(() => undefined);
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [readVersions]);

  async function install(version: string) {
    await run(`install:${version}`, () => installApp("temurin", version));
  }

  async function remove(version: string) {
    await run(`uninstall:${version}`, async () => {
      if (selected.some((record) => record.appId === "temurin" && record.version === version)) {
        await clearSelection("temurin");
      }
      await uninstallApp("temurin", version);
    });
  }

  const javaRows = buildJavaVersionRows(versions, installed, selected);
  const selectedVersion = selected.find((record) => record.appId === "temurin")?.version;

  function uninstallControl(version: string, isSelected: boolean, uninstalling: boolean) {
    return (
      <Dialog.Root>
        <Dialog.Trigger asChild>
          <Button
            aria-label={t("runtimePage.uninstallAria", {
              app: "Java",
              version,
            })}
            disabled={uninstalling}
            size="sm"
            variant="danger"
          >
            {uninstalling ? <RefreshCw className="spin" size={14} /> : <Trash2 size={14} />}
            {uninstalling ? t("runtimePage.uninstalling") : t("runtimePage.uninstall")}
          </Button>
        </Dialog.Trigger>
        <Dialog.Portal>
          <Dialog.Overlay className="dialog-overlay" />
          <Dialog.Content className="dialog-content">
            <Dialog.Title>
              {t("runtimePage.uninstallTitle", {
                app: "Java",
                version,
              })}
            </Dialog.Title>
            <Dialog.Description>
              {isSelected
                ? t("runtimePage.uninstallSelectedDescription")
                : t("runtimePage.uninstallDescription")}
            </Dialog.Description>
            <div className="dialog-actions">
              <Dialog.Close asChild>
                <Button variant="ghost">{t("common.cancel")}</Button>
              </Dialog.Close>
              <Dialog.Close asChild>
                <Button onClick={() => void remove(version)} variant="danger">
                  {t("runtimePage.uninstall")}
                </Button>
              </Dialog.Close>
            </div>
          </Dialog.Content>
        </Dialog.Portal>
      </Dialog.Root>
    );
  }

  return (
    <div className="page-stack">
      {error ? (
        <div className="error-banner" role="alert">
          <CircleAlert size={16} /> {error}
        </div>
      ) : null}
      <div className="detail-grid">
        <Card className="version-panel">
          <div className="section-heading">
            <div>
              <span className="eyebrow">{t("runtimePage.officialReleases")}</span>
              <h2>{t("runtimePage.availableVersions")}</h2>
            </div>
            {selectedVersion ? (
              <Button
                disabled={busy.has("clear-selection")}
                onClick={() => void run("clear-selection", () => clearSelection("temurin"))}
                size="sm"
                variant="secondary"
              >
                {t("runtimePage.clearSelection")}
              </Button>
            ) : null}
          </div>
          {loading ? (
            <div className="skeleton-list">
              <span />
              <span />
              <span />
            </div>
          ) : (
            <div className="version-list">
              {javaRows.length === 0 ? (
                <p className="version-catalog-status">{t("runtimePage.catalogUpdating")}</p>
              ) : null}
              {javaRows.map((row) => {
                const installAction = `install:${row.version}`;
                const operationEvent = activeRuntimeOperation(operations, "temurin", row.version);
                const installEvent =
                  operationEvent?.kind === "install" ? operationEvent : undefined;
                const uninstallEvent =
                  operationEvent?.kind === "uninstall" ? operationEvent : undefined;
                const installing = busy.has(installAction) || Boolean(installEvent);
                const uninstalling =
                  busy.has(`uninstall:${row.version}`) || Boolean(uninstallEvent);
                return (
                  <div className="version-row runtime-version-row" key={row.version}>
                    <div className="version-main">
                      <strong>JDK {row.channel}</strong>
                      <Badge tone="accent">LTS</Badge>
                      <span className="runtime-version-summary">v{row.version}</span>
                    </div>
                    <span className="release-date">
                      {row.available?.releasedAt.slice(0, 10) ?? ""}
                    </span>
                    <span className="version-actions">
                      {row.installed ? (
                        <Badge tone="positive">
                          <Check size={12} /> {t("runtimePage.installed")}
                        </Badge>
                      ) : row.available ? (
                        <Button
                          disabled={installing}
                          onClick={() => void install(row.available?.version ?? "")}
                          size="sm"
                          variant="secondary"
                        >
                          {installing ? (
                            <RefreshCw className="spin" size={14} />
                          ) : (
                            <ArrowDownToLine size={14} />
                          )}{" "}
                          {installing ? t("common.installing") : t("common.install")}
                        </Button>
                      ) : null}
                      {row.installed ? (
                        row.selected ? (
                          <Badge tone="accent">{t("runtimePage.selected")}</Badge>
                        ) : (
                          <Button
                            disabled={busy.has(`select:${row.installed.version}`)}
                            onClick={() =>
                              void run(`select:${row.installed?.version}`, () =>
                                selectPrimary(row.installed?.version ?? ""),
                              )
                            }
                            size="sm"
                          >
                            {busy.has(`select:${row.installed.version}`)
                              ? t("runtimePage.selecting")
                              : t("runtimePage.select")}
                          </Button>
                        )
                      ) : null}
                      {row.installed
                        ? uninstallControl(row.installed.version, row.selected, uninstalling)
                        : null}
                    </span>
                    <RuntimeOperationProgress
                      event={operationEvent}
                      pending={installing || uninstalling}
                      pendingLabel={uninstalling ? t("runtimePage.uninstalling") : undefined}
                      version={row.version}
                    />
                  </div>
                );
              })}
            </div>
          )}
        </Card>
      </div>
    </div>
  );
}

function latestOperationEvents(events: OperationEvent[]) {
  const latest = new Map<string, OperationEvent>();
  for (const event of events) {
    const current = latest.get(event.operationId);
    if (!current || event.sequence > current.sequence) {
      latest.set(event.operationId, event);
    }
  }
  return Array.from(latest.values()).sort((left, right) =>
    right.timestamp.localeCompare(left.timestamp),
  );
}

export function LogsPage({
  events,
  onChanged,
  cancel = cancelOperation,
}: {
  events: OperationEvent[];
  onChanged: () => Promise<void>;
  cancel?: (operationId: string) => Promise<void>;
}) {
  const { t } = useTranslation();
  const [error, setError] = useState<string | null>(null);
  const [requested, setRequested] = useState<Set<string>>(() => new Set());
  const operations = latestOperationEvents(events);

  async function requestCancellation(operationId: string) {
    setError(null);
    setRequested((current) => new Set(current).add(operationId));
    try {
      await cancel(operationId);
      await onChanged();
    } catch (reason) {
      setRequested((current) => {
        const next = new Set(current);
        next.delete(operationId);
        return next;
      });
      setError(formatTorbenError(reason));
    }
  }

  return (
    <div className="page-stack">
      <PageHeader
        description={t("logsPage.description")}
        eyebrow={t("logsPage.eyebrow")}
        title={t("logsPage.title")}
      />
      {error ? (
        <div className="error-banner" role="alert">
          <CircleAlert size={16} /> {error}
        </div>
      ) : null}
      {operations.length ? (
        <Card className="activity-list">
          {operations.map((event) => (
            <OperationRow
              cancelRequested={requested.has(event.operationId)}
              event={event}
              key={event.operationId}
              onCancel={
                event.state === "running"
                  ? () => void requestCancellation(event.operationId)
                  : undefined
              }
            />
          ))}
        </Card>
      ) : (
        <EmptyState description={t("logsPage.emptyDescription")} title={t("logsPage.emptyTitle")} />
      )}
    </div>
  );
}

function OperationRow({
  event,
  onCancel,
  cancelRequested = false,
}: {
  event: OperationEvent;
  onCancel?: () => void;
  cancelRequested?: boolean;
}) {
  const { t, i18n: translation } = useTranslation();
  const progress = event.progress === undefined ? 0 : event.progress * 100;
  const stateLabel = t(`logsPage.state.${event.state}`);
  const stateTone =
    event.state === "succeeded"
      ? "positive"
      : event.state === "running"
        ? "accent"
        : event.state === "pending"
          ? "neutral"
          : "warning";
  return (
    <div className="operation-row">
      <div aria-hidden="true" className={`operation-state state-${event.state}`}>
        {event.state === "succeeded" ? <Check size={14} /> : <Activity size={14} />}
      </div>
      <div className="operation-copy">
        <div>
          <strong>{event.phase}</strong>
          <span>{event.message}</span>
        </div>
        {event.state === "running" ? (
          <ProgressBar label={t("logsPage.progress", { phase: event.phase })} value={progress} />
        ) : null}
      </div>
      <div className="operation-meta">
        <Badge tone={stateTone}>{stateLabel}</Badge>
        <time>
          {formatTimestamp(event.timestamp, translation.resolvedLanguage ?? translation.language)}
        </time>
        {onCancel ? (
          <Button disabled={cancelRequested} onClick={onCancel} size="sm" variant="ghost">
            {cancelRequested ? t("logsPage.cancelling") : t("logsPage.cancel")}
          </Button>
        ) : null}
      </div>
    </div>
  );
}

function initialSchemaValues(pages: SchemaPage[]): Record<string, string> {
  return Object.fromEntries(
    pages.flatMap((page) =>
      page.sections.flatMap((section) =>
        section.fields
          .filter((field) => !field.readOnly)
          .map((field) => [schemaValueKey(page.id, field.id), field.value ?? ""]),
      ),
    ),
  );
}

function activePageSchemaValues(
  page: SchemaPage,
  values: Record<string, string>,
): Record<string, string> {
  return Object.fromEntries(
    page.sections.flatMap((section) =>
      section.fields
        .filter((field) => !field.readOnly)
        .map((field) => [field.id, values[schemaValueKey(page.id, field.id)] ?? field.value ?? ""]),
    ),
  );
}

function schemaValueKey(pageId: string, fieldId: string) {
  return `${pageId}:${fieldId}`;
}

interface PluginsPageProps {
  plugins: PluginSummary[];
  pluginOrder?: string[];
  registry?: PluginRegistryStatus;
  onChanged: () => Promise<void>;
  onPluginOrderChange?: (pluginOrder: string[]) => Promise<void>;
  chooseManifest?: () => Promise<string | null>;
  installManifest?: (manifestPath: string, developerMode: boolean) => Promise<PluginSummary>;
  onInstallBundledTemurin?: () => Promise<PluginSummary>;
  onUninstallBundledTemurin?: () => Promise<void>;
  onInstallBundledNode?: () => Promise<PluginSummary>;
  onUninstallBundledNode?: () => Promise<void>;
  onInstallBundledPython?: () => Promise<PluginSummary>;
  onUninstallBundledPython?: () => Promise<void>;
  onInstallBundledRust?: () => Promise<PluginSummary>;
  onUninstallBundledRust?: () => Promise<void>;
  onInstallBundledMysql?: () => Promise<PluginSummary>;
  onUninstallBundledMysql?: () => Promise<void>;
  onInstallBundledRedis?: () => Promise<PluginSummary>;
  onUninstallBundledRedis?: () => Promise<void>;
  onInstallBundledPostgresql?: () => Promise<PluginSummary>;
  onUninstallBundledPostgresql?: () => Promise<void>;
  installRegistryPlugin?: (pluginId: string, version?: string) => Promise<PluginSummary>;
  refreshRegistry?: () => Promise<PluginRegistryStatus>;
  runSchemaAction?: (
    pluginId: string,
    pageId: string,
    sectionId: string,
    actionId: string,
    values: Record<string, string>,
    confirmed: boolean,
  ) => Promise<SchemaActionResult>;
  changeEnabled?: (pluginId: string, enabled: boolean) => Promise<void>;
}

const defaultPluginOrder: string[] = [];

export function PluginsPage({
  plugins,
  pluginOrder: savedPluginOrder = defaultPluginOrder,
  registry = {
    configured: false,
    sourceUrl: null,
    cachePath: "",
    sequence: null,
    generatedAt: null,
  },
  onChanged,
  onPluginOrderChange = async () => undefined,
  chooseManifest = choosePluginManifest,
  installManifest = installPlugin,
  onInstallBundledTemurin = async () => {
    throw new Error("Bundled Temurin installation is unavailable.");
  },
  onUninstallBundledTemurin = async () => {
    throw new Error("Bundled Temurin uninstall is unavailable.");
  },
  onInstallBundledNode = async () => {
    throw new Error("Bundled Node.js installation is unavailable.");
  },
  onUninstallBundledNode = async () => {
    throw new Error("Bundled Node.js uninstall is unavailable.");
  },
  onInstallBundledPython = async () => {
    throw new Error("Bundled Python installation is unavailable.");
  },
  onUninstallBundledPython = async () => {
    throw new Error("Bundled Python uninstall is unavailable.");
  },
  onInstallBundledRust = async () => {
    throw new Error("Bundled Rust installation is unavailable.");
  },
  onUninstallBundledRust = async () => {
    throw new Error("Bundled Rust uninstall is unavailable.");
  },
  onInstallBundledMysql = async () => {
    throw new Error("Bundled MySQL installation is unavailable.");
  },
  onUninstallBundledMysql = async () => {
    throw new Error("Bundled MySQL uninstall is unavailable.");
  },
  onInstallBundledRedis = async () => {
    throw new Error("Bundled Redis installation is unavailable.");
  },
  onUninstallBundledRedis = async () => {
    throw new Error("Bundled Redis uninstall is unavailable.");
  },
  onInstallBundledPostgresql = async () => {
    throw new Error("Bundled PostgreSQL installation is unavailable.");
  },
  onUninstallBundledPostgresql = async () => {
    throw new Error("Bundled PostgreSQL uninstall is unavailable.");
  },
  installRegistryPlugin = installOfficialPluginFromRegistry,
  refreshRegistry = refreshOfficialPluginRegistry,
  runSchemaAction = invokePluginSchemaAction,
  changeEnabled = setPluginEnabled,
}: PluginsPageProps) {
  const { t, i18n: translation } = useTranslation();
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [installOpen, setInstallOpen] = useState(false);
  const [officialInstallOpen, setOfficialInstallOpen] = useState(false);
  const [officialPluginId, setOfficialPluginId] = useState("");
  const [officialVersion, setOfficialVersion] = useState("");
  const [schemaPlugin, setSchemaPlugin] = useState<PluginSummary | null>(null);
  const [schemaPages, setSchemaPages] = useState<SchemaPage[]>([]);
  const [selectedSchemaPage, setSelectedSchemaPage] = useState<string | null>(null);
  const [schemaValues, setSchemaValues] = useState<Record<string, string>>({});
  const [schemaMessage, setSchemaMessage] = useState<string | null>(null);
  const [pendingSchemaAction, setPendingSchemaAction] = useState<{
    section: SchemaSection;
    action: SchemaAction;
  } | null>(null);
  const [uninstallPlugin, setUninstallPlugin] = useState<PluginSummary | null>(null);
  const [pluginOrder, setPluginOrder] = useState<string[]>(() =>
    normalizePluginOrder(savedPluginOrder, plugins),
  );
  const [draggingPlugin, setDraggingPlugin] = useState<string | null>(null);
  const [dropTargetPlugin, setDropTargetPlugin] = useState<string | null>(null);
  const [dragPointer, setDragPointer] = useState<{ x: number; y: number } | null>(null);
  const draggingPluginRef = useRef<string | null>(null);

  useEffect(() => {
    setPluginOrder(normalizePluginOrder(savedPluginOrder, plugins));
  }, [plugins, savedPluginOrder]);

  useEffect(() => {
    const finishPointerDrag = () => {
      draggingPluginRef.current = null;
      setDraggingPlugin(null);
      setDropTargetPlugin(null);
      setDragPointer(null);
    };
    const trackPointerDrag = (event: PointerEvent) => {
      if (draggingPluginRef.current) {
        setDragPointer({ x: event.clientX, y: event.clientY });
      }
    };
    window.addEventListener("pointerup", finishPointerDrag);
    window.addEventListener("pointercancel", finishPointerDrag);
    window.addEventListener("pointermove", trackPointerDrag);
    return () => {
      window.removeEventListener("pointerup", finishPointerDrag);
      window.removeEventListener("pointercancel", finishPointerDrag);
      window.removeEventListener("pointermove", trackPointerDrag);
    };
  }, []);

  function reorderPlugin(pluginId: string, targetPluginId: string) {
    if (pluginId === targetPluginId) return;
    const source = normalizePluginOrder(pluginOrder, plugins);
    const next = movePlugin(source, pluginId, targetPluginId);
    if (next.every((value, index) => value === source[index])) return;
    setPluginOrder(next);
    void onPluginOrderChange(next).catch((reason: unknown) => {
      setError(formatTorbenError(reason));
      setPluginOrder(normalizePluginOrder(savedPluginOrder, plugins));
    });
  }

  async function installSideloadedPlugin() {
    setBusy("install");
    setError(null);
    try {
      const manifestPath = await chooseManifest();
      if (!manifestPath) {
        setInstallOpen(false);
        return;
      }
      await installManifest(manifestPath, true);
      await onChanged();
      setInstallOpen(false);
    } catch (reason) {
      setError(formatTorbenError(reason));
    } finally {
      setBusy(null);
    }
  }

  async function installBundledPlugin(plugin: PluginSummary) {
    setBusy(plugin.id);
    setError(null);
    try {
      if (plugin.id === "app.torben.plugin.node") await onInstallBundledNode();
      else if (plugin.id === "app.torben.plugin.python") await onInstallBundledPython();
      else if (plugin.id === "app.torben.plugin.rust") await onInstallBundledRust();
      else if (plugin.id === "app.torben.plugin.mysql") await onInstallBundledMysql();
      else if (plugin.id === "app.torben.plugin.redis") await onInstallBundledRedis();
      else if (plugin.id === "app.torben.plugin.postgresql") await onInstallBundledPostgresql();
      else await onInstallBundledTemurin();
      await onChanged();
    } catch (reason) {
      setError(formatTorbenError(reason));
    } finally {
      setBusy(null);
    }
  }

  async function uninstallBundledPlugin(plugin: PluginSummary) {
    setBusy(plugin.id);
    setError(null);
    try {
      if (plugin.id === "app.torben.plugin.node") await onUninstallBundledNode();
      else if (plugin.id === "app.torben.plugin.python") await onUninstallBundledPython();
      else if (plugin.id === "app.torben.plugin.rust") await onUninstallBundledRust();
      else if (plugin.id === "app.torben.plugin.mysql") await onUninstallBundledMysql();
      else if (plugin.id === "app.torben.plugin.redis") await onUninstallBundledRedis();
      else if (plugin.id === "app.torben.plugin.postgresql") await onUninstallBundledPostgresql();
      else await onUninstallBundledTemurin();
      await onChanged();
      setUninstallPlugin(null);
    } catch (reason) {
      setError(formatTorbenError(reason));
      setUninstallPlugin(null);
    } finally {
      setBusy(null);
    }
  }

  async function togglePlugin(plugin: PluginSummary) {
    setBusy(plugin.id);
    setError(null);
    try {
      await changeEnabled(plugin.id, !plugin.enabled);
      await onChanged();
    } catch (reason) {
      setError(formatTorbenError(reason));
    } finally {
      setBusy(null);
    }
  }

  async function refreshOfficialRegistry() {
    setBusy("registry-refresh");
    setError(null);
    try {
      await refreshRegistry();
      await onChanged();
    } catch (reason) {
      setError(formatTorbenError(reason));
    } finally {
      setBusy(null);
    }
  }

  async function installFromOfficialRegistry() {
    const pluginId = officialPluginId.trim();
    if (!pluginId) return;
    setBusy("registry-install");
    setError(null);
    try {
      await installRegistryPlugin(pluginId, officialVersion.trim() || undefined);
      await onChanged();
      setOfficialInstallOpen(false);
      setOfficialPluginId("");
      setOfficialVersion("");
    } catch (reason) {
      setError(formatTorbenError(reason));
    } finally {
      setBusy(null);
    }
  }

  function closeSchemaPages() {
    setSchemaPlugin(null);
    setSchemaPages([]);
    setSelectedSchemaPage(null);
    setSchemaValues({});
    setSchemaMessage(null);
    setPendingSchemaAction(null);
  }

  async function invokeSchemaAction(
    section: SchemaSection,
    action: SchemaAction,
    confirmed: boolean,
  ) {
    if (!schemaPlugin || !selectedSchemaPage) return;
    if (action.kind === "destructive" && !confirmed) {
      setPendingSchemaAction({ section, action });
      return;
    }
    setBusy(`schema-action:${section.id}:${action.id}`);
    setError(null);
    try {
      const activePage = schemaPages.find((page) => page.id === selectedSchemaPage);
      if (!activePage) return;
      const result = await runSchemaAction(
        schemaPlugin.id,
        selectedSchemaPage,
        section.id,
        action.id,
        activePageSchemaValues(activePage, schemaValues),
        confirmed,
      );
      const pages = schemaPages.map((page) => (page.id === result.page.id ? result.page : page));
      setSchemaPages(pages);
      setSchemaValues(initialSchemaValues(pages));
      setSchemaMessage(result.message);
      setPendingSchemaAction(null);
      await onChanged();
    } catch (reason) {
      setError(formatTorbenError(reason));
    } finally {
      setBusy(null);
    }
  }

  const activeSchemaPage = schemaPages.find((page) => page.id === selectedSchemaPage) ?? null;
  const orderedPlugins = [...plugins].sort((left, right) =>
    comparePluginOrder(left.id, right.id, pluginOrder),
  );

  return (
    <div className="page-stack">
      {draggingPlugin && dragPointer ? (
        <div
          aria-hidden="true"
          className="plugin-drag-preview"
          style={{ left: dragPointer.x + 14, top: dragPointer.y + 14 }}
        >
          <GripVertical size={15} />
          <span>
            {orderedPlugins.find((plugin) => plugin.id === draggingPlugin)?.displayName ??
              draggingPlugin}
          </span>
        </div>
      ) : null}
      <PageHeader
        description={t("pluginsPage.description")}
        eyebrow={t("pluginsPage.eyebrow")}
        title={t("pluginsPage.title")}
        actions={
          <Dialog.Root onOpenChange={setInstallOpen} open={installOpen}>
            <Dialog.Trigger asChild>
              <Button disabled={Boolean(busy)}>
                <ArrowDownToLine size={15} /> {t("pluginsPage.installPlugin")}
              </Button>
            </Dialog.Trigger>
            <Dialog.Portal>
              <Dialog.Overlay className="dialog-overlay" />
              <Dialog.Content className="dialog-content">
                <Dialog.Title>{t("pluginsPage.developerTitle")}</Dialog.Title>
                <Dialog.Description>{t("pluginsPage.developerDescription")}</Dialog.Description>
                <ul className="developer-checklist">
                  <li>{t("pluginsPage.developerRegistryWarning")}</li>
                  <li>{t("pluginsPage.developerVerification")}</li>
                </ul>
                <div className="dialog-actions">
                  <Button
                    disabled={busy === "install"}
                    onClick={() => setInstallOpen(false)}
                    variant="ghost"
                  >
                    {t("common.cancel")}
                  </Button>
                  <Button
                    disabled={busy === "install"}
                    onClick={() => void installSideloadedPlugin()}
                  >
                    {busy === "install" ? t("common.installing") : t("pluginsPage.chooseManifest")}
                  </Button>
                </div>
              </Dialog.Content>
            </Dialog.Portal>
          </Dialog.Root>
        }
      />
      {error ? (
        <div className="error-banner" role="alert">
          <CircleAlert size={16} /> {error}
        </div>
      ) : null}
      <details className="plugin-registry-disclosure">
        <summary>
          <ShieldCheck size={22} />
          <h2>{t("pluginsPage.registryTitle")}</h2>
          <Badge tone={registry.configured ? "positive" : "warning"}>
            {registry.configured ? t("pluginsPage.configured") : t("pluginsPage.developmentBuild")}
          </Badge>
        </summary>
        <div className="registry-card-body">
          <p>
            {registry.sequence === null
              ? registry.configured
                ? t("pluginsPage.noSnapshot")
                : t("pluginsPage.noTrustRoot")
              : t("pluginsPage.trustedSequence", {
                  sequence: registry.sequence,
                  time: registry.generatedAt
                    ? formatTimestamp(
                        registry.generatedAt,
                        translation.resolvedLanguage ?? translation.language,
                      )
                    : t("pluginsPage.unknownTime"),
                })}
          </p>
          {registry.sourceUrl ? <code>{registry.sourceUrl}</code> : null}
        </div>
        <div className="registry-actions plugin-registry-actions">
          <Button
            disabled={!registry.configured || Boolean(busy)}
            onClick={() => void refreshOfficialRegistry()}
            size="sm"
            variant="secondary"
          >
            <RefreshCw size={14} />
            {busy === "registry-refresh" ? t("common.refreshing") : t("common.refresh")}
          </Button>
          <Dialog.Root onOpenChange={setOfficialInstallOpen} open={officialInstallOpen}>
            <Dialog.Trigger asChild>
              <Button disabled={!registry.configured || Boolean(busy)} size="sm">
                <ArrowDownToLine size={14} /> {t("pluginsPage.installOfficial")}
              </Button>
            </Dialog.Trigger>
            <Dialog.Portal>
              <Dialog.Overlay className="dialog-overlay" />
              <Dialog.Content className="dialog-content">
                <Dialog.Title>{t("pluginsPage.officialDialogTitle")}</Dialog.Title>
                <Dialog.Description>
                  {t("pluginsPage.officialDialogDescription")}
                </Dialog.Description>
                <label className="dialog-field">
                  <span>{t("pluginsPage.pluginId")}</span>
                  <input
                    autoComplete="off"
                    onChange={(event) => setOfficialPluginId(event.target.value)}
                    placeholder="app.example.plugin"
                    value={officialPluginId}
                  />
                </label>
                <label className="dialog-field">
                  <span>{t("pluginsPage.exactVersion")}</span>
                  <input
                    autoComplete="off"
                    onChange={(event) => setOfficialVersion(event.target.value)}
                    placeholder={t("pluginsPage.latestVersion")}
                    value={officialVersion}
                  />
                </label>
                <div className="dialog-actions">
                  <Button
                    disabled={busy === "registry-install"}
                    onClick={() => setOfficialInstallOpen(false)}
                    variant="ghost"
                  >
                    {t("common.cancel")}
                  </Button>
                  <Button
                    disabled={!officialPluginId.trim() || busy === "registry-install"}
                    onClick={() => void installFromOfficialRegistry()}
                  >
                    {busy === "registry-install" ? t("common.installing") : t("common.install")}
                  </Button>
                </div>
              </Dialog.Content>
            </Dialog.Portal>
          </Dialog.Root>
        </div>
      </details>
      <div className="plugin-list">
        {orderedPlugins.map((plugin) => {
          const builtIn = plugin.origin === "built_in";
          const temurin = plugin.id === "app.torben.plugin.temurin";
          const installableBundled = plugin.id in bundledRuntimePages;
          const pluginDisplayName = temurin ? "Java" : plugin.displayName;
          const bundledAppId = bundledApplicationIds[plugin.id];
          return (
            <Card
              className={`plugin-card${plugin.enabled ? "" : " is-disabled"}${draggingPlugin === plugin.id ? " is-dragging" : ""}${dropTargetPlugin === plugin.id ? " is-drop-target" : ""}`}
              data-plugin-id={plugin.id}
              key={plugin.id}
              onPointerEnter={() => {
                if (draggingPluginRef.current && draggingPluginRef.current !== plugin.id) {
                  setDropTargetPlugin(plugin.id);
                }
              }}
              onPointerUp={() => {
                const sourcePluginId = draggingPluginRef.current;
                if (sourcePluginId && sourcePluginId !== plugin.id) {
                  reorderPlugin(sourcePluginId, plugin.id);
                }
                draggingPluginRef.current = null;
                setDraggingPlugin(null);
                setDropTargetPlugin(null);
              }}
            >
              <button
                aria-label={t("pluginsPage.reorderPluginAria", { plugin: pluginDisplayName })}
                className="plugin-drag-handle"
                onKeyDown={(event) => {
                  const index = orderedPlugins.findIndex((entry) => entry.id === plugin.id);
                  const target =
                    event.key === "ArrowUp"
                      ? orderedPlugins[index - 1]
                      : event.key === "ArrowDown"
                        ? orderedPlugins[index + 1]
                        : undefined;
                  if (target) {
                    event.preventDefault();
                    reorderPlugin(plugin.id, target.id);
                  }
                }}
                onPointerDown={(event) => {
                  if (event.button !== 0) return;
                  event.preventDefault();
                  draggingPluginRef.current = plugin.id;
                  setDraggingPlugin(plugin.id);
                  setDragPointer({ x: event.clientX, y: event.clientY });
                  setDropTargetPlugin(null);
                }}
                title={t("pluginsPage.reorderPluginHint")}
                type="button"
              >
                <GripVertical aria-hidden="true" size={16} />
              </button>
              <a
                aria-label={t("pluginsPage.detailsAria", { plugin: pluginDisplayName })}
                className="plugin-card-main"
                href={`#/plugins/${encodeURIComponent(plugin.id)}`}
              >
                <div
                  className={
                    builtIn && bundledAppId ? `app-icon app-icon-${bundledAppId}` : "app-icon"
                  }
                >
                  {builtIn && bundledAppId ? (
                    <AppGlyph id={bundledAppId} />
                  ) : (
                    pluginDisplayName.slice(0, 2).toUpperCase()
                  )}
                </div>
                <div>
                  <div className="app-card-title">
                    <h2>{pluginDisplayName}</h2>
                  </div>
                  <p className="plugin-summary">{pluginSummary(t, plugin)}</p>
                </div>
              </a>
              <div className="plugin-card-actions">
                {installableBundled ? (
                  plugin.enabled ? (
                    <Button
                      aria-label={t("pluginsPage.uninstallPluginAria", {
                        plugin: pluginDisplayName,
                      })}
                      disabled={Boolean(busy)}
                      onClick={() => setUninstallPlugin(plugin)}
                      size="sm"
                      variant="danger"
                    >
                      <Trash2 size={14} /> {t("pluginsPage.uninstallPlugin")}
                    </Button>
                  ) : (
                    <Button
                      aria-label={t("pluginsPage.installBundledAria", {
                        plugin: pluginDisplayName,
                      })}
                      disabled={Boolean(busy)}
                      onClick={() => void installBundledPlugin(plugin)}
                      size="sm"
                    >
                      <ArrowDownToLine size={14} />
                      {busy === plugin.id
                        ? t("common.installing")
                        : t("pluginsPage.installBundled")}
                    </Button>
                  )
                ) : (
                  <Button
                    aria-label={
                      builtIn && plugin.enabled
                        ? t("pluginsPage.bundledAria", { plugin: pluginDisplayName })
                        : t("pluginsPage.toggleAria", {
                            action: plugin.enabled ? t("pluginsPage.disable") : t("common.enable"),
                            plugin: pluginDisplayName,
                          })
                    }
                    disabled={(builtIn && plugin.enabled) || Boolean(busy)}
                    onClick={() => void togglePlugin(plugin)}
                    size="sm"
                    variant="secondary"
                  >
                    {busy === plugin.id
                      ? t("common.updating")
                      : builtIn && plugin.enabled
                        ? t("pluginsPage.bundled")
                        : plugin.enabled
                          ? t("pluginsPage.disable")
                          : t("common.enable")}
                  </Button>
                )}
              </div>
            </Card>
          );
        })}
        {!plugins.length ? (
          <EmptyState
            description={t("pluginsPage.noPluginsDescription")}
            title={t("pluginsPage.noPluginsTitle")}
          />
        ) : null}
      </div>
      <Dialog.Root
        onOpenChange={(open) => {
          if (!open && !busy) setUninstallPlugin(null);
        }}
        open={uninstallPlugin !== null}
      >
        <Dialog.Portal>
          <Dialog.Overlay className="dialog-overlay" />
          <Dialog.Content className="dialog-content">
            <Dialog.Title>
              {t("pluginsPage.uninstallConfirmTitle", {
                plugin:
                  uninstallPlugin?.id === "app.torben.plugin.temurin"
                    ? "Java"
                    : uninstallPlugin?.displayName,
              })}
            </Dialog.Title>
            <Dialog.Description>
              {t("pluginsPage.uninstallConfirmDescription", {
                application:
                  uninstallPlugin?.id === "app.torben.plugin.node"
                    ? "Node.js"
                    : uninstallPlugin?.id === "app.torben.plugin.python"
                      ? t("pluginsPage.pythonRuntime")
                      : uninstallPlugin?.id === "app.torben.plugin.temurin"
                        ? "JDK"
                        : uninstallPlugin?.displayName,
              })}
            </Dialog.Description>
            <div className="dialog-actions">
              <Button
                disabled={Boolean(busy)}
                onClick={() => setUninstallPlugin(null)}
                variant="ghost"
              >
                {t("common.cancel")}
              </Button>
              <Button
                disabled={Boolean(busy)}
                onClick={() => {
                  if (uninstallPlugin) void uninstallBundledPlugin(uninstallPlugin);
                }}
                variant="danger"
              >
                {busy ? t("common.updating") : t("pluginsPage.uninstallPlugin")}
              </Button>
            </div>
          </Dialog.Content>
        </Dialog.Portal>
      </Dialog.Root>
      <Dialog.Root
        onOpenChange={(open) => {
          if (!open) closeSchemaPages();
        }}
        open={schemaPlugin !== null}
      >
        <Dialog.Portal>
          <Dialog.Overlay className="dialog-overlay" />
          <Dialog.Content className="dialog-content schema-dialog">
            <Dialog.Title>
              {t("pluginsPage.pagesTitle", {
                plugin: schemaPlugin ? displayPluginName(schemaPlugin) : t("plugins"),
              })}
            </Dialog.Title>
            <Dialog.Description>{t("pluginsPage.schemaDescription")}</Dialog.Description>
            {schemaPages.length > 1 ? (
              <div className="schema-tabs" role="tablist">
                {schemaPages.map((page) => (
                  <Button
                    aria-selected={page.id === selectedSchemaPage}
                    key={page.id}
                    onClick={() => {
                      setSelectedSchemaPage(page.id);
                      setPendingSchemaAction(null);
                      setSchemaMessage(null);
                    }}
                    role="tab"
                    size="sm"
                    variant={page.id === selectedSchemaPage ? "primary" : "ghost"}
                  >
                    {page.title}
                  </Button>
                ))}
              </div>
            ) : null}
            {activeSchemaPage ? (
              <div className="schema-page">
                <div className="schema-page-heading">
                  <h3>{activeSchemaPage.title}</h3>
                  {activeSchemaPage.description ? <p>{activeSchemaPage.description}</p> : null}
                </div>
                {activeSchemaPage.sections.map((section) => (
                  <section className="schema-section" key={section.id}>
                    {section.title ? <h4>{section.title}</h4> : null}
                    {section.description ? <p>{section.description}</p> : null}
                    <div className="schema-fields">
                      {section.fields.map((field) => (
                        <div className="schema-field" key={field.id}>
                          {field.readOnly || field.kind === "status" ? (
                            <span className="schema-field-label">{field.label}</span>
                          ) : (
                            <label
                              className="schema-field-label"
                              htmlFor={`schema-field-${field.id}`}
                            >
                              {field.label}
                              {field.required ? " *" : ""}
                            </label>
                          )}
                          {field.description ? <small>{field.description}</small> : null}
                          {field.readOnly || field.kind === "status" ? (
                            <span className={`schema-value schema-value-${field.kind}`}>
                              {field.value ?? "—"}
                            </span>
                          ) : field.kind === "boolean" ? (
                            <input
                              checked={
                                (schemaValues[schemaValueKey(activeSchemaPage.id, field.id)] ??
                                  field.value) === "true"
                              }
                              id={`schema-field-${field.id}`}
                              onChange={(event) =>
                                setSchemaValues((values) => ({
                                  ...values,
                                  [schemaValueKey(activeSchemaPage.id, field.id)]: String(
                                    event.target.checked,
                                  ),
                                }))
                              }
                              required={field.required}
                              type="checkbox"
                            />
                          ) : field.kind === "select" ? (
                            <select
                              id={`schema-field-${field.id}`}
                              onChange={(event) =>
                                setSchemaValues((values) => ({
                                  ...values,
                                  [schemaValueKey(activeSchemaPage.id, field.id)]:
                                    event.target.value,
                                }))
                              }
                              required={field.required}
                              value={
                                schemaValues[schemaValueKey(activeSchemaPage.id, field.id)] ??
                                field.value ??
                                ""
                              }
                            >
                              {field.options.map((option) => (
                                <option key={option.value} value={option.value}>
                                  {option.label}
                                </option>
                              ))}
                            </select>
                          ) : (
                            <input
                              id={`schema-field-${field.id}`}
                              onChange={(event) =>
                                setSchemaValues((values) => ({
                                  ...values,
                                  [schemaValueKey(activeSchemaPage.id, field.id)]:
                                    event.target.value,
                                }))
                              }
                              placeholder={field.placeholder ?? undefined}
                              required={field.required}
                              type="text"
                              value={
                                schemaValues[schemaValueKey(activeSchemaPage.id, field.id)] ??
                                field.value ??
                                ""
                              }
                            />
                          )}
                        </div>
                      ))}
                    </div>
                    {section.actions.length ? (
                      <div className="schema-actions">
                        {section.actions.map((action) => (
                          <Button
                            disabled={!action.enabled || Boolean(busy)}
                            key={action.id}
                            onClick={() => void invokeSchemaAction(section, action, false)}
                            size="sm"
                            variant={
                              action.kind === "destructive"
                                ? "danger"
                                : action.kind === "secondary"
                                  ? "secondary"
                                  : "primary"
                            }
                          >
                            {busy === `schema-action:${section.id}:${action.id}`
                              ? t("pluginsPage.working")
                              : action.label}
                          </Button>
                        ))}
                      </div>
                    ) : null}
                  </section>
                ))}
                {schemaMessage ? <div className="schema-message">{schemaMessage}</div> : null}
              </div>
            ) : (
              <EmptyState
                description={t("pluginsPage.noPagesDescription")}
                title={t("pluginsPage.noPagesTitle")}
              />
            )}
            {pendingSchemaAction ? (
              <div className="schema-confirmation" role="alert">
                <strong>{t("pluginsPage.confirmDestructive")}</strong>
                <p>{pendingSchemaAction.action.confirmation}</p>
                <div className="dialog-actions">
                  <Button onClick={() => setPendingSchemaAction(null)} size="sm" variant="ghost">
                    {t("common.cancel")}
                  </Button>
                  <Button
                    onClick={() =>
                      void invokeSchemaAction(
                        pendingSchemaAction.section,
                        pendingSchemaAction.action,
                        true,
                      )
                    }
                    size="sm"
                    variant="danger"
                  >
                    {t("common.confirm")}
                  </Button>
                </div>
              </div>
            ) : null}
            <div className="dialog-actions">
              <Button disabled={Boolean(busy)} onClick={closeSchemaPages} variant="ghost">
                {t("common.close")}
              </Button>
            </div>
          </Dialog.Content>
        </Dialog.Portal>
      </Dialog.Root>
    </div>
  );
}

async function choosePluginManifest(): Promise<string | null> {
  const selected = await open({
    directory: false,
    multiple: false,
    filters: [{ name: i18n.t("pluginsPage.manifestFilter"), extensions: ["json"] }],
  });
  if (Array.isArray(selected)) {
    return selected[0] ?? null;
  }
  return selected;
}

function PluginPermissionList({ permissions }: { permissions: PluginPermissions }) {
  const { t } = useTranslation();
  const items = [
    permissions.networkDomains.length ? (
      <span key="network">
        <ExternalLink size={13} /> {permissions.networkDomains.join(" · ")}
      </span>
    ) : null,
    permissions.filesystemRoots.length ? (
      <span key="filesystem">
        <HardDrive size={13} /> {permissions.filesystemRoots.join(" · ")}
      </span>
    ) : null,
    permissions.externalCommands.length ? (
      <span key="commands">
        <TerminalSquare size={13} /> {permissions.externalCommands.join(" · ")}
      </span>
    ) : null,
    permissions.packageManagers.length ? (
      <span key="package-managers">
        <PackageCheck size={13} /> {permissions.packageManagers.join(" · ")}
      </span>
    ) : null,
  ].filter(Boolean);
  return (
    <div className="permission-row">
      {items.length ? (
        items
      ) : (
        <span>
          <ShieldCheck size={13} /> {t("pluginsPage.noPermissions")}
        </span>
      )}
    </div>
  );
}

function pluginSummary(t: (key: string) => string, plugin: PluginSummary) {
  const keyById: Record<string, string> = {
    "app.torben.plugin.node": "pluginsPage.summaryNode",
    "app.torben.plugin.temurin": "pluginsPage.summaryJava",
    "app.torben.plugin.python": "pluginsPage.summaryPython",
    "app.torben.plugin.rust": "pluginsPage.summaryRust",
    "app.torben.plugin.mysql": "pluginsPage.summaryMysql",
    "app.torben.plugin.redis": "pluginsPage.summaryRedis",
    "app.torben.plugin.postgresql": "pluginsPage.summaryPostgresql",
  };
  return t(keyById[plugin.id] ?? "pluginsPage.summaryPlugin");
}

export function PluginDetailPage({
  plugin,
  loadSchemaPages = getPluginSchemaPages,
  onSettingsChange,
  settings,
}: {
  plugin: PluginSummary | null;
  loadSchemaPages?: (pluginId: string) => Promise<SchemaPage[]>;
  onSettingsChange?: (settings: UserSettings) => Promise<void>;
  settings?: UserSettings;
}) {
  const { t } = useTranslation();
  const [schemaPages, setSchemaPages] = useState<SchemaPage[]>([]);
  const [loading, setLoading] = useState(false);
  const [environmentRows, setEnvironmentRows] = useState<EnvironmentVariableRow[]>([]);
  const [environmentSaving, setEnvironmentSaving] = useState(false);
  const [environmentError, setEnvironmentError] = useState<string | null>(null);
  const [environmentMessage, setEnvironmentMessage] = useState<string | null>(null);

  useEffect(() => {
    if (!plugin) return;
    let disposed = false;
    setLoading(true);
    void loadSchemaPages(plugin.id)
      .then((pages) => {
        if (!disposed) setSchemaPages(pages);
      })
      .catch(() => {
        if (!disposed) setSchemaPages([]);
      })
      .finally(() => {
        if (!disposed) setLoading(false);
      });
    return () => {
      disposed = true;
    };
  }, [loadSchemaPages, plugin]);

  const environmentAppId = plugin ? pluginEnvironmentAppIds[plugin.id] : undefined;
  useEffect(() => {
    const variables =
      environmentAppId && settings
        ? (settings.applicationEnvironments[environmentAppId] ?? {})
        : {};
    setEnvironmentRows(environmentVariableRows(variables));
    setEnvironmentError(null);
    setEnvironmentMessage(null);
  }, [environmentAppId, settings]);

  async function savePluginEnvironment() {
    if (!environmentAppId || !settings || !onSettingsChange) return;
    const variables: Record<string, string> = {};
    const normalizedNames = new Set<string>();
    for (const row of environmentRows) {
      const name = row.name.trim();
      if (!name && !row.value) continue;
      if (!/^[A-Za-z_][A-Za-z0-9_]{0,127}$/.test(name)) {
        setEnvironmentError(t("pluginsPage.environmentNameInvalid"));
        return;
      }
      const normalized = name.toLocaleUpperCase("en-US");
      if (normalizedNames.has(normalized)) {
        setEnvironmentError(t("pluginsPage.environmentNameDuplicate", { name }));
        return;
      }
      normalizedNames.add(normalized);
      variables[name] = row.value;
    }
    setEnvironmentSaving(true);
    setEnvironmentError(null);
    setEnvironmentMessage(null);
    try {
      const applicationEnvironments = { ...settings.applicationEnvironments };
      if (Object.keys(variables).length) applicationEnvironments[environmentAppId] = variables;
      else delete applicationEnvironments[environmentAppId];
      await onSettingsChange({ ...settings, applicationEnvironments });
      setEnvironmentMessage(t("pluginsPage.environmentSaved"));
    } catch (reason) {
      setEnvironmentError(formatTorbenError(reason));
    } finally {
      setEnvironmentSaving(false);
    }
  }

  if (!plugin) {
    return (
      <EmptyState
        description={t("pluginsPage.noPluginsDescription")}
        title={t("pluginsPage.noPluginsTitle")}
      />
    );
  }
  const name = displayPluginName(plugin);
  const appId = bundledApplicationIds[plugin.id];
  const runtimePage = bundledRuntimePages[plugin.id];
  return (
    <div className="page-stack plugin-detail-page">
      <PageHeader
        description={pluginSummary(t, plugin)}
        eyebrow={t("pluginsPage.detailEyebrow")}
        title={name}
        actions={
          <Button asChild variant="secondary">
            <Link to="/plugins">
              <ArrowRight size={14} /> {t("pluginsPage.backToPlugins")}
            </Link>
          </Button>
        }
      />
      <Card className="plugin-detail-hero">
        <div className={`app-icon app-icon-${appId ?? "plugin"}`}>
          {appId ? <AppGlyph id={appId} /> : name.slice(0, 2).toUpperCase()}
        </div>
        <div>
          <div className="app-card-title">
            <h2>{name}</h2>
            <Badge tone={plugin.enabled ? "positive" : "warning"}>
              {plugin.enabled ? t("common.enabled") : t("pluginsPage.availableToInstall")}
            </Badge>
          </div>
          <p className="plugin-summary">{pluginSummary(t, plugin)}</p>
        </div>
        {runtimePage && plugin.enabled ? (
          <Button asChild>
            <Link to={runtimePage}>
              <Wrench size={14} /> {t("pluginsPage.open")}
            </Link>
          </Button>
        ) : null}
      </Card>
      <div className="plugin-detail-grid">
        <Card className="plugin-detail-section">
          <h2>{t("pluginsPage.detailInformation")}</h2>
          <dl className="plugin-detail-facts">
            <div>
              <dt>{t("pluginsPage.publisher")}</dt>
              <dd>{plugin.publisher}</dd>
            </div>
            <div>
              <dt>{t("pluginsPage.version")}</dt>
              <dd>{plugin.version}</dd>
            </div>
            <div>
              <dt>{t("pluginsPage.source")}</dt>
              <dd>
                {plugin.origin === "built_in"
                  ? t("pluginsPage.builtIn")
                  : plugin.origin === "official_registry"
                    ? t("pluginsPage.officialRegistry")
                    : t("pluginsPage.sideloaded")}
              </dd>
            </div>
            <div>
              <dt>{t("pluginsPage.capabilities")}</dt>
              <dd>{plugin.capabilities.join(" · ")}</dd>
            </div>
          </dl>
        </Card>
        <Card className="plugin-detail-section">
          <h2>{t("pluginsPage.permissions")}</h2>
          <PluginPermissionList permissions={plugin.permissions} />
        </Card>
      </div>
      {environmentAppId && settings && onSettingsChange ? (
        <Card className="plugin-detail-section plugin-environment-section">
          <div className="plugin-environment-heading">
            <div>
              <h2>{t("pluginsPage.environmentVariables")}</h2>
              <p>{t("pluginsPage.environmentDescription")}</p>
            </div>
            <Badge tone="accent">{t("pluginsPage.processScoped")}</Badge>
          </div>
          <datalist id={`environment-suggestions-${environmentAppId}`}>
            {(environmentVariableSuggestions[environmentAppId] ?? []).map((name) => (
              <option key={name} value={name} />
            ))}
          </datalist>
          <div className="plugin-environment-list">
            {environmentRows.map((row, index) => (
              <div className="plugin-environment-row" key={row.id}>
                <label>
                  <span>{t("pluginsPage.environmentName")}</span>
                  <input
                    aria-label={t("pluginsPage.environmentNameAria", { index: index + 1 })}
                    autoComplete="off"
                    list={`environment-suggestions-${environmentAppId}`}
                    onChange={(event) =>
                      setEnvironmentRows((current) =>
                        current.map((entry) =>
                          entry.id === row.id ? { ...entry, name: event.target.value } : entry,
                        ),
                      )
                    }
                    placeholder={
                      environmentVariableSuggestions[environmentAppId]?.[0] ?? "VARIABLE_NAME"
                    }
                    spellCheck={false}
                    value={row.name}
                  />
                </label>
                <label>
                  <span>{t("pluginsPage.environmentValue")}</span>
                  <input
                    aria-label={t("pluginsPage.environmentValueAria", { index: index + 1 })}
                    autoComplete="off"
                    onChange={(event) =>
                      setEnvironmentRows((current) =>
                        current.map((entry) =>
                          entry.id === row.id ? { ...entry, value: event.target.value } : entry,
                        ),
                      )
                    }
                    placeholder={t("pluginsPage.environmentValuePlaceholder")}
                    spellCheck={false}
                    value={row.value}
                  />
                </label>
                <Button
                  aria-label={t("pluginsPage.removeEnvironmentVariable", { index: index + 1 })}
                  onClick={() =>
                    setEnvironmentRows((current) => current.filter((entry) => entry.id !== row.id))
                  }
                  size="sm"
                  variant="ghost"
                >
                  <Trash2 size={14} />
                </Button>
              </div>
            ))}
          </div>
          {!environmentRows.length ? (
            <p className="plugin-metadata">{t("pluginsPage.noEnvironmentVariables")}</p>
          ) : null}
          <p className="plugin-environment-note">{t("pluginsPage.environmentStorageNote")}</p>
          {environmentError ? (
            <p className="settings-message error" role="alert">
              {environmentError}
            </p>
          ) : null}
          {environmentMessage ? (
            <p className="settings-message" role="status">
              {environmentMessage}
            </p>
          ) : null}
          <div className="plugin-environment-actions">
            <Button
              disabled={environmentSaving || environmentRows.length >= 32}
              onClick={() =>
                setEnvironmentRows((current) => [
                  ...current,
                  { id: nextEnvironmentVariableRowId++, name: "", value: "" },
                ])
              }
              size="sm"
              variant="secondary"
            >
              <Plus size={14} /> {t("pluginsPage.addEnvironmentVariable")}
            </Button>
            <Button
              disabled={environmentSaving}
              onClick={() => void savePluginEnvironment()}
              size="sm"
            >
              <Save size={14} />
              {environmentSaving
                ? t("pluginsPage.savingEnvironment")
                : t("pluginsPage.saveEnvironment")}
            </Button>
          </div>
        </Card>
      ) : null}
      <Card className="plugin-detail-section">
        <h2>{t("pluginsPage.pluginPages")}</h2>
        {loading ? (
          <p className="plugin-metadata">{t("pluginsPage.opening")}</p>
        ) : schemaPages.length ? (
          schemaPages.map((page) => (
            <section className="plugin-detail-schema" key={page.id}>
              <h3>{page.title}</h3>
              {page.description ? <p>{page.description}</p> : null}
              {page.sections.map((section) => (
                <div key={section.id}>
                  <h4>{section.title}</h4>
                  {section.description ? <p>{section.description}</p> : null}
                  {section.fields.length ? (
                    <ul>
                      {section.fields.map((field) => (
                        <li key={field.id}>
                          <strong>{field.label}</strong>
                          <span>{field.value ?? "—"}</span>
                        </li>
                      ))}
                    </ul>
                  ) : null}
                </div>
              ))}
            </section>
          ))
        ) : (
          <p className="plugin-metadata">{t("pluginsPage.noPagesDescription")}</p>
        )}
      </Card>
    </div>
  );
}

export function DiagnosticsPage({
  checks,
  sourceAdapters,
  applications = [],
  packageInstallations = [],
  installed = [],
  onChanged,
  planSource = planSourceOperation,
  executeSource = executeSourceOperation,
  planMigration = planSourceMigration,
  executeMigration = executeSourceMigration,
  planManagedMigration = planManagedToPackageMigration,
  executeManagedMigration = executeManagedToPackageMigration,
  planPackageMigration = planPackageToManagedMigration,
  executePackageMigration = executePackageToManagedMigration,
}: {
  checks: DoctorCheck[];
  sourceAdapters: SourceAdapterStatus[];
  applications?: ApplicationDescriptor[];
  packageInstallations?: PackageInstallationRecord[];
  installed?: InstallRecord[];
  onChanged: () => Promise<void>;
  planSource?: typeof planSourceOperation;
  executeSource?: (request: SourceExecutionRequest) => Promise<SourceExecutionResult>;
  planMigration?: (request: SourceMigrationRequest) => Promise<SourceMigrationPlan>;
  executeMigration?: (request: SourceMigrationRequest) => Promise<SourceMigrationResult>;
  planManagedMigration?: (
    request: SourceMigrationRequest,
  ) => Promise<ManagedToPackageMigrationPlan>;
  executeManagedMigration?: (
    request: SourceMigrationRequest,
  ) => Promise<ManagedToPackageMigrationResult>;
  planPackageMigration?: (
    request: PackageToManagedMigrationRequest,
  ) => Promise<PackageToManagedMigrationPlan>;
  executePackageMigration?: (
    request: PackageToManagedMigrationRequest,
  ) => Promise<PackageToManagedMigrationResult>;
}) {
  const { t } = useTranslation();
  const [refreshing, setRefreshing] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [sourceAction, setSourceAction] = useState<
    SourceAction | "migrate" | "managed-to-package" | "package-to-managed"
  >("install");
  const [sourceApp, setSourceApp] = useState(applications[0]?.id ?? "node");
  const [sourceAppVersion, setSourceAppVersion] = useState("");
  const defaultSourceAdapter =
    sourceAdapters.find((source) => source.availability === "available")?.adapter ?? "winget";
  const [sourceAdapter, setSourceAdapter] = useState<SourceAdapterKind>(defaultSourceAdapter);
  const [sourceCoordinate, setSourceCoordinate] = useState("");
  const [sourcePackageKind, setSourcePackageKind] = useState<SourcePackageKind>(
    defaultSourceAdapter === "homebrew" ? "formula" : "native",
  );
  const [sourcePackageVersion, setSourcePackageVersion] = useState("");
  const [sourceExecutable, setSourceExecutable] = useState("");
  const [sourcePlan, setSourcePlan] = useState<SourceOperationPlan | null>(null);
  const [migrationPlan, setMigrationPlan] = useState<SourceMigrationPlan | null>(null);
  const [managedMigrationPlan, setManagedMigrationPlan] =
    useState<ManagedToPackageMigrationPlan | null>(null);
  const [packageMigrationPlan, setPackageMigrationPlan] =
    useState<PackageToManagedMigrationPlan | null>(null);
  const [sourceAccepted, setSourceAccepted] = useState(false);
  const [sourceBusy, setSourceBusy] = useState<"plan" | "execute" | null>(null);
  const [sourceMessage, setSourceMessage] = useState<string | null>(null);
  const [sourceConfirmOpen, setSourceConfirmOpen] = useState(false);

  function invalidateSourcePlan() {
    setSourcePlan(null);
    setMigrationPlan(null);
    setManagedMigrationPlan(null);
    setPackageMigrationPlan(null);
    setSourceAccepted(false);
    setSourceMessage(null);
  }

  async function refresh() {
    setRefreshing(true);
    setError(null);
    try {
      await onChanged();
    } catch (reason) {
      setError(formatTorbenError(reason));
    } finally {
      setRefreshing(false);
    }
  }

  async function reviewSourcePlan() {
    setSourceBusy("plan");
    setError(null);
    setSourceMessage(null);
    try {
      if (sourceAction === "package-to-managed") {
        const plan = await planPackageMigration(packageToManagedRequest(null, false));
        setPackageMigrationPlan(plan);
        setManagedMigrationPlan(null);
        setMigrationPlan(null);
        setSourcePlan(null);
      } else if (sourceAction === "managed-to-package") {
        const plan = await planManagedMigration(sourceMigrationRequest(null, false));
        setPackageMigrationPlan(null);
        setManagedMigrationPlan(plan);
        setMigrationPlan(null);
        setSourcePlan(null);
      } else if (sourceAction === "migrate") {
        const plan = await planMigration(sourceMigrationRequest(null, false));
        setMigrationPlan(plan);
        setPackageMigrationPlan(null);
        setManagedMigrationPlan(null);
        setSourcePlan(null);
      } else {
        const plan = await planSource(
          sourceAction,
          sourceAdapter,
          sourceCoordinate,
          sourcePackageKind,
          sourcePackageVersion.trim() || null,
        );
        setSourcePlan(plan);
        setPackageMigrationPlan(null);
        setMigrationPlan(null);
        setManagedMigrationPlan(null);
      }
      setSourceAccepted(false);
    } catch (reason) {
      setError(formatTorbenError(reason));
    } finally {
      setSourceBusy(null);
    }
  }

  async function executeReviewedSourcePlan() {
    if (
      (!sourcePlan && !migrationPlan && !managedMigrationPlan && !packageMigrationPlan) ||
      !sourceAccepted
    )
      return;
    setSourceBusy("execute");
    setError(null);
    setSourceMessage(null);
    try {
      if (sourceAction === "package-to-managed" && packageMigrationPlan) {
        await executePackageMigration(
          packageToManagedRequest(packageMigrationPlan.approvalToken, true),
        );
      } else if (sourceAction === "managed-to-package" && managedMigrationPlan) {
        await executeManagedMigration(
          sourceMigrationRequest(managedMigrationPlan.approvalToken, true),
        );
      } else if (sourceAction === "migrate" && migrationPlan) {
        await executeMigration(sourceMigrationRequest(migrationPlan.approvalToken, true));
      } else if (sourcePlan && (sourceAction === "install" || sourceAction === "uninstall")) {
        await executeSource({
          appId: sourceApp,
          appVersion: sourceAppVersion,
          action: sourceAction,
          adapter: sourceAdapter,
          coordinate: sourceCoordinate,
          packageKind: sourcePackageKind,
          packageVersion: sourcePackageVersion.trim() || null,
          executablePath: sourceAction === "install" ? sourceExecutable.trim() || null : null,
          approvedExecutionIdentity: sourcePlan.executionIdentity,
          acceptSystemChanges: true,
        });
      }
      await onChanged();
      setSourcePlan(null);
      setMigrationPlan(null);
      setManagedMigrationPlan(null);
      setPackageMigrationPlan(null);
      setSourceAccepted(false);
      setSourceMessage(t("diagnosticsPage.sourceOperations.completed"));
    } catch (reason) {
      setError(formatTorbenError(reason));
    } finally {
      setSourceBusy(null);
      setSourceConfirmOpen(false);
    }
  }

  function sourceMigrationRequest(
    approvedPlanToken: string | null,
    acceptSystemChanges: boolean,
  ): SourceMigrationRequest {
    return {
      appId: sourceApp,
      appVersion: sourceAppVersion,
      targetAdapter: sourceAdapter,
      targetCoordinate: sourceCoordinate,
      targetPackageKind: sourcePackageKind,
      targetPackageVersion: sourcePackageVersion.trim() || null,
      targetExecutablePath: sourceExecutable.trim(),
      approvedPlanToken,
      acceptSystemChanges,
    };
  }

  function packageToManagedRequest(
    approvedPlanToken: string | null,
    acceptSystemChanges: boolean,
  ): PackageToManagedMigrationRequest {
    return {
      appId: sourceApp,
      appVersion: sourceAppVersion,
      approvedPlanToken,
      acceptSystemChanges,
    };
  }

  function prepareOwnedUninstall(record: PackageInstallationRecord) {
    setSourceAction("uninstall");
    setSourceApp(record.appId);
    setSourceAppVersion(record.appVersion);
    setSourceAdapter(record.adapter);
    setSourceCoordinate(record.coordinate);
    setSourcePackageKind(record.packageKind);
    setSourcePackageVersion(record.packageVersion);
    setSourceExecutable("");
    invalidateSourcePlan();
  }

  function prepareOwnedMigration(record: PackageInstallationRecord) {
    setSourceAction("migrate");
    setSourceApp(record.appId);
    setSourceAppVersion(record.appVersion);
    setSourceCoordinate("");
    setSourcePackageVersion(record.packageVersion);
    setSourceExecutable("");
    invalidateSourcePlan();
  }

  function preparePackageToManagedMigration(record: PackageInstallationRecord) {
    setSourceAction("package-to-managed");
    setSourceApp(record.appId);
    setSourceAppVersion(record.appVersion);
    setSourceAdapter(record.adapter);
    setSourceCoordinate(record.coordinate);
    setSourcePackageKind(record.packageKind);
    setSourcePackageVersion(record.packageVersion);
    setSourceExecutable(record.executablePath);
    invalidateSourcePlan();
  }

  function prepareManagedMigration(record: InstallRecord) {
    setSourceAction("managed-to-package");
    setSourceApp(record.appId);
    setSourceAppVersion(record.version);
    setSourceCoordinate("");
    setSourcePackageVersion(record.version);
    setSourceExecutable("");
    invalidateSourcePlan();
  }

  const reviewedSourcePlans = packageMigrationPlan
    ? ([
        [
          t("diagnosticsPage.sourceOperations.removeCurrent"),
          packageMigrationPlan.uninstallCurrent,
        ],
        [t("diagnosticsPage.sourceOperations.restoreCurrent"), packageMigrationPlan.restoreCurrent],
      ] as const)
    : managedMigrationPlan
      ? ([
          [t("diagnosticsPage.sourceOperations.installTarget"), managedMigrationPlan.installTarget],
          [t("diagnosticsPage.sourceOperations.cleanupTarget"), managedMigrationPlan.cleanupTarget],
        ] as const)
      : migrationPlan
        ? ([
            [t("diagnosticsPage.sourceOperations.removeCurrent"), migrationPlan.uninstallCurrent],
            [t("diagnosticsPage.sourceOperations.installTarget"), migrationPlan.installTarget],
            [t("diagnosticsPage.sourceOperations.cleanupTarget"), migrationPlan.cleanupTarget],
            [t("diagnosticsPage.sourceOperations.restoreCurrent"), migrationPlan.restoreCurrent],
          ] as const)
        : sourcePlan
          ? ([[t("diagnosticsPage.sourceOperations.command"), sourcePlan]] as const)
          : [];
  const reviewedWarnings =
    packageMigrationPlan?.warnings ??
    managedMigrationPlan?.warnings ??
    migrationPlan?.warnings ??
    sourcePlan?.warnings ??
    [];
  const reviewedIdentity =
    packageMigrationPlan?.approvalToken ??
    managedMigrationPlan?.approvalToken ??
    migrationPlan?.approvalToken ??
    sourcePlan?.executionIdentity ??
    null;
  const managedInstallations = installed.filter((record) => record.scope === "managed");

  return (
    <div className="page-stack">
      <PageHeader
        description={t("diagnosticsPage.description")}
        eyebrow={t("diagnosticsPage.eyebrow")}
        title={t("diagnosticsPage.title")}
        actions={
          <Button disabled={refreshing} onClick={() => void refresh()} variant="secondary">
            <RefreshCw size={15} />
            {refreshing ? t("diagnosticsPage.checking") : t("diagnosticsPage.runChecks")}
          </Button>
        }
      />
      {error ? (
        <div className="error-banner" role="alert">
          {error}
        </div>
      ) : null}
      <Card className="diagnostic-list">
        {checks.map((check) => (
          <div className="diagnostic-row" key={check.id}>
            <span className={check.healthy ? "check-positive" : "check-negative"}>
              {check.healthy ? <CheckCircle2 size={18} /> : <CircleAlert size={18} />}
            </span>
            <div>
              <strong>{check.id.replaceAll("_", " ")}</strong>
              <p>{check.message}</p>
            </div>
            <Badge tone={check.healthy ? "positive" : "warning"}>
              {check.healthy ? t("diagnosticsPage.passed") : t("diagnosticsPage.attention")}
            </Badge>
          </div>
        ))}
      </Card>
      <div className="section-heading">
        <div>
          <span className="eyebrow">{t("diagnosticsPage.systemSources")}</span>
          <h2>{t("diagnosticsPage.packageManagers")}</h2>
          <p>{t("diagnosticsPage.planningDescription")}</p>
        </div>
        <Badge tone="accent">{t("diagnosticsPage.planningOnly")}</Badge>
      </div>
      <Card className="diagnostic-list">
        {sourceAdapters.map((source) => {
          const available = source.availability === "available";
          const missing = source.availability === "missing";
          return (
            <div className="diagnostic-row" key={source.adapter}>
              <span
                className={
                  available ? "check-positive" : missing ? "check-negative" : "check-neutral"
                }
              >
                {available ? <CheckCircle2 size={18} /> : <CircleAlert size={18} />}
              </span>
              <div>
                <strong>{source.adapter}</strong>
                <p>{source.version ?? source.message}</p>
                <p>
                  {source.supportsExactVersion
                    ? t("diagnosticsPage.exactPlanning")
                    : t("diagnosticsPage.coordinateOnly")}
                  {source.requiresElevation
                    ? ` · ${t("diagnosticsPage.externalAuthorization")}`
                    : ` · ${t("diagnosticsPage.userLevelPlan")}`}
                </p>
              </div>
              <Badge tone={available ? "positive" : missing ? "warning" : undefined}>
                {available
                  ? t("common.available")
                  : missing
                    ? t("diagnosticsPage.missing")
                    : t("diagnosticsPage.unsupported")}
              </Badge>
            </div>
          );
        })}
      </Card>
      <div className="section-heading">
        <div>
          <span className="eyebrow">{t("diagnosticsPage.sourceOperations.eyebrow")}</span>
          <h2>{t("diagnosticsPage.sourceOperations.title")}</h2>
          <p>{t("diagnosticsPage.sourceOperations.description")}</p>
        </div>
      </div>
      <Card className="source-ownership-card">
        <h3>{t("diagnosticsPage.sourceOperations.ownedTitle")}</h3>
        {packageInstallations.length || managedInstallations.length ? (
          <div className="source-ownership-list">
            {managedInstallations.map((record) => (
              <div
                className="source-ownership-row"
                key={`managed-${record.appId}@${record.version}`}
              >
                <div>
                  <strong>
                    {record.appId}@{record.version}
                  </strong>
                  <p>
                    {t("diagnosticsPage.sourceOperations.managedSource")} · {record.sourceId}
                  </p>
                </div>
                <Button
                  disabled={Boolean(sourceBusy)}
                  onClick={() => prepareManagedMigration(record)}
                  size="sm"
                  variant="secondary"
                >
                  <ArrowRight size={14} />{" "}
                  {t("diagnosticsPage.sourceOperations.preparePackageMigration")}
                </Button>
              </div>
            ))}
            {packageInstallations.map((record) => (
              <div className="source-ownership-row" key={`${record.appId}@${record.appVersion}`}>
                <div>
                  <strong>
                    {record.appId}@{record.appVersion}
                  </strong>
                  <p>
                    {record.adapter} · {record.coordinate} · {record.packageVersion}
                  </p>
                </div>
                <div className="source-operation-actions">
                  <Button
                    disabled={Boolean(sourceBusy)}
                    onClick={() => preparePackageToManagedMigration(record)}
                    size="sm"
                    variant="secondary"
                  >
                    <FolderArchive size={14} />{" "}
                    {t("diagnosticsPage.sourceOperations.prepareManagedMigration")}
                  </Button>
                  <Button
                    disabled={Boolean(sourceBusy)}
                    onClick={() => prepareOwnedMigration(record)}
                    size="sm"
                    variant="secondary"
                  >
                    <ArrowRight size={14} />{" "}
                    {t("diagnosticsPage.sourceOperations.prepareMigration")}
                  </Button>
                  <Button
                    disabled={Boolean(sourceBusy)}
                    onClick={() => prepareOwnedUninstall(record)}
                    size="sm"
                    variant="secondary"
                  >
                    <Trash2 size={14} /> {t("diagnosticsPage.sourceOperations.prepareUninstall")}
                  </Button>
                </div>
              </div>
            ))}
          </div>
        ) : (
          <p>{t("diagnosticsPage.sourceOperations.noOwned")}</p>
        )}
      </Card>
      <Card className="source-operation-card">
        <form
          onSubmit={(event) => {
            event.preventDefault();
            void reviewSourcePlan();
          }}
        >
          <div className="source-operation-grid">
            <label className="dialog-field">
              <span>{t("diagnosticsPage.sourceOperations.action")}</span>
              <select
                onChange={(event) => {
                  setSourceAction(
                    event.target.value as
                      | SourceAction
                      | "migrate"
                      | "managed-to-package"
                      | "package-to-managed",
                  );
                  invalidateSourcePlan();
                }}
                value={sourceAction}
              >
                <option value="install">{t("diagnosticsPage.sourceOperations.install")}</option>
                <option value="uninstall">{t("diagnosticsPage.sourceOperations.uninstall")}</option>
                <option value="migrate">{t("diagnosticsPage.sourceOperations.migrate")}</option>
                <option value="managed-to-package">
                  {t("diagnosticsPage.sourceOperations.managedToPackage")}
                </option>
                <option value="package-to-managed">
                  {t("diagnosticsPage.sourceOperations.packageToManaged")}
                </option>
              </select>
            </label>
            <label className="dialog-field">
              <span>{t("diagnosticsPage.sourceOperations.app")}</span>
              <select
                onChange={(event) => {
                  setSourceApp(event.target.value);
                  invalidateSourcePlan();
                }}
                value={sourceApp}
              >
                {applications.map((application) => (
                  <option key={application.id} value={application.id}>
                    {application.displayName}
                  </option>
                ))}
              </select>
            </label>
            <label className="dialog-field">
              <span>{t("diagnosticsPage.sourceOperations.appVersion")}</span>
              <input
                onChange={(event) => {
                  setSourceAppVersion(event.target.value);
                  invalidateSourcePlan();
                }}
                placeholder="1.134.0"
                required
                value={sourceAppVersion}
              />
            </label>
            {sourceAction !== "package-to-managed" ? (
              <>
                <label className="dialog-field">
                  <span>{t("diagnosticsPage.sourceOperations.adapter")}</span>
                  <select
                    onChange={(event) => {
                      const adapter = event.target.value as SourceAdapterKind;
                      setSourceAdapter(adapter);
                      setSourcePackageKind(adapter === "homebrew" ? "formula" : "native");
                      invalidateSourcePlan();
                    }}
                    value={sourceAdapter}
                  >
                    {sourceAdapters.map((source) => (
                      <option
                        disabled={source.availability !== "available"}
                        key={source.adapter}
                        value={source.adapter}
                      >
                        {source.adapter}
                      </option>
                    ))}
                  </select>
                </label>
                <label className="dialog-field">
                  <span>{t("diagnosticsPage.sourceOperations.coordinate")}</span>
                  <input
                    onChange={(event) => {
                      setSourceCoordinate(event.target.value);
                      invalidateSourcePlan();
                    }}
                    placeholder="Microsoft.VisualStudioCode"
                    required
                    value={sourceCoordinate}
                  />
                </label>
                <label className="dialog-field">
                  <span>{t("diagnosticsPage.sourceOperations.packageKind")}</span>
                  <select
                    disabled={sourceAdapter !== "homebrew"}
                    onChange={(event) => {
                      setSourcePackageKind(event.target.value as SourcePackageKind);
                      invalidateSourcePlan();
                    }}
                    value={sourcePackageKind}
                  >
                    {sourceAdapter === "homebrew" ? (
                      <>
                        <option value="formula">
                          {t("diagnosticsPage.sourceOperations.formula")}
                        </option>
                        <option value="cask">{t("diagnosticsPage.sourceOperations.cask")}</option>
                      </>
                    ) : (
                      <option value="native">{t("diagnosticsPage.sourceOperations.native")}</option>
                    )}
                  </select>
                </label>
                <label className="dialog-field">
                  <span>{t("diagnosticsPage.sourceOperations.packageVersion")}</span>
                  <input
                    onChange={(event) => {
                      setSourcePackageVersion(event.target.value);
                      invalidateSourcePlan();
                    }}
                    placeholder="1.134.0"
                    required={sourceAction !== "uninstall" && sourceAdapter !== "homebrew"}
                    value={sourcePackageVersion}
                  />
                </label>
              </>
            ) : null}
            {sourceAction !== "uninstall" && sourceAction !== "package-to-managed" ? (
              <label className="dialog-field">
                <span>{t("diagnosticsPage.sourceOperations.executablePath")}</span>
                <input
                  onChange={(event) => {
                    setSourceExecutable(event.target.value);
                    invalidateSourcePlan();
                  }}
                  placeholder="C:\\...\\code.exe"
                  required
                  value={sourceExecutable}
                />
              </label>
            ) : null}
          </div>
          <div className="source-operation-actions">
            <Button disabled={Boolean(sourceBusy)} type="submit" variant="secondary">
              <Wrench size={14} />
              {sourceBusy === "plan"
                ? t("diagnosticsPage.sourceOperations.reviewing")
                : t("diagnosticsPage.sourceOperations.reviewPlan")}
            </Button>
          </div>
        </form>
        {sourcePlan || migrationPlan || managedMigrationPlan || packageMigrationPlan ? (
          <div className="source-plan">
            <h3>{t("diagnosticsPage.sourceOperations.planTitle")}</h3>
            <dl>
              {managedMigrationPlan ? (
                <div>
                  <dt>{t("diagnosticsPage.sourceOperations.stageManaged")}</dt>
                  <dd>
                    <code>{managedMigrationPlan.currentInstallation.installPath}</code>
                  </dd>
                </div>
              ) : null}
              {packageMigrationPlan ? (
                <div>
                  <dt>{t("diagnosticsPage.sourceOperations.installManaged")}</dt>
                  <dd>
                    <code>{packageMigrationPlan.managedTargetPath}</code>
                  </dd>
                </div>
              ) : null}
              {reviewedSourcePlans.map(([label, plan]) => (
                <div key={label}>
                  <dt>{label}</dt>
                  <dd>
                    <code>
                      {plan.executable} {plan.executeArguments.join(" ")}
                    </code>
                  </dd>
                </div>
              ))}
              <div>
                <dt>{t("diagnosticsPage.sourceOperations.requiresElevation")}</dt>
                <dd>
                  {reviewedSourcePlans.some(([, plan]) => plan.requiresElevation)
                    ? t("diagnosticsPage.sourceOperations.yes")
                    : t("diagnosticsPage.sourceOperations.no")}
                </dd>
              </div>
              <div>
                <dt>{t("diagnosticsPage.sourceOperations.exactVersion")}</dt>
                <dd>
                  {reviewedSourcePlans.every(([, plan]) => plan.exactVersionGuaranteed)
                    ? t("diagnosticsPage.sourceOperations.yes")
                    : t("diagnosticsPage.sourceOperations.no")}
                </dd>
              </div>
            </dl>
            <strong>{t("diagnosticsPage.sourceOperations.warnings")}</strong>
            <ul>
              {reviewedWarnings.map((warning) => (
                <li key={warning}>{warning}</li>
              ))}
            </ul>
            {reviewedIdentity ? (
              <div className="source-plan-identity">
                <strong>
                  {migrationPlan || managedMigrationPlan || packageMigrationPlan
                    ? t("diagnosticsPage.sourceOperations.approvalToken")
                    : t("diagnosticsPage.sourceOperations.executionIdentity")}
                </strong>
                <code>{reviewedIdentity}</code>
              </div>
            ) : null}
            <label className="source-plan-accept">
              <input
                checked={sourceAccepted}
                onChange={(event) => setSourceAccepted(event.target.checked)}
                type="checkbox"
              />
              <span>{t("diagnosticsPage.sourceOperations.acceptLabel")}</span>
            </label>
            <Dialog.Root onOpenChange={setSourceConfirmOpen} open={sourceConfirmOpen}>
              <Dialog.Trigger asChild>
                <Button disabled={!sourceAccepted || Boolean(sourceBusy)}>
                  <PackageCheck size={14} /> {t("diagnosticsPage.sourceOperations.executeChange")}
                </Button>
              </Dialog.Trigger>
              <Dialog.Portal>
                <Dialog.Overlay className="dialog-overlay" />
                <Dialog.Content className="dialog-content">
                  <Dialog.Title>{t("diagnosticsPage.sourceOperations.executeTitle")}</Dialog.Title>
                  <Dialog.Description>
                    {t("diagnosticsPage.sourceOperations.executeDescription")}
                  </Dialog.Description>
                  {reviewedSourcePlans.map(([label, plan]) => (
                    <code className="source-confirm-command" key={label}>
                      {plan.executable} {plan.executeArguments.join(" ")}
                    </code>
                  ))}
                  <div className="dialog-actions">
                    <Button
                      disabled={sourceBusy === "execute"}
                      onClick={() => setSourceConfirmOpen(false)}
                      variant="ghost"
                    >
                      {t("common.cancel")}
                    </Button>
                    <Button
                      disabled={sourceBusy === "execute"}
                      onClick={() => void executeReviewedSourcePlan()}
                    >
                      {sourceBusy === "execute"
                        ? t("diagnosticsPage.sourceOperations.executing")
                        : t("diagnosticsPage.sourceOperations.executeChange")}
                    </Button>
                  </div>
                </Dialog.Content>
              </Dialog.Portal>
            </Dialog.Root>
          </div>
        ) : null}
        {sourceMessage ? <div className="schema-message">{sourceMessage}</div> : null}
      </Card>
    </div>
  );
}

export function SettingsPage({
  settings,
  onChange = updateSettings,
  shellIntegration,
  onShellChange = setShellIntegration,
  managedLibrary = {
    path: "Platform data directory/apps",
    defaultPath: "Platform data directory/apps",
    custom: false,
    bytesUsed: 0,
  },
  onLibraryMigrate,
  updater = {
    configured: false,
    currentVersion: "0.0.1",
    endpoint: "",
  },
  updateStatus = {
    state: "unconfigured",
    currentVersion: "0.0.1",
    availableVersion: null,
    publishedAt: null,
    notes: null,
    progress: null,
    message: null,
  },
  onUpdateCheck = async () => undefined,
  onInstallTorbenUpdate = async () => undefined,
}: {
  settings: UserSettings;
  onChange?: (settings: UserSettings) => Promise<void>;
  shellIntegration: ShellIntegrationStatus;
  onShellChange?: (enabled: boolean) => Promise<unknown>;
  managedLibrary?: ManagedLibraryStatus;
  onLibraryMigrate?: (targetPath: string) => Promise<ManagedLibraryMigrationResult>;
  updater?: DesktopUpdaterConfiguration;
  updateStatus?: TorbenUpdateStatus;
  onUpdateCheck?: () => Promise<void>;
  onInstallTorbenUpdate?: () => Promise<void>;
}) {
  const { t, i18n: translation } = useTranslation();
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [shellSaving, setShellSaving] = useState(false);
  const [shellError, setShellError] = useState<string | null>(null);
  const [librarySaving, setLibrarySaving] = useState(false);
  const [libraryError, setLibraryError] = useState<string | null>(null);
  const [libraryResult, setLibraryResult] = useState<ManagedLibraryMigrationResult | null>(null);

  async function changeSettings(next: UserSettings) {
    setSaving(true);
    setError(null);
    try {
      await onChange(next);
    } catch (reason) {
      setError(formatTorbenError(reason));
    } finally {
      setSaving(false);
    }
  }

  async function changeShellIntegration(enabled: boolean) {
    setShellSaving(true);
    setShellError(null);
    try {
      await onShellChange(enabled);
    } catch (reason) {
      setShellError(formatTorbenError(reason));
    } finally {
      setShellSaving(false);
    }
  }

  async function chooseLibraryTarget() {
    if (!onLibraryMigrate) {
      return;
    }
    const selected = await open({ directory: true, multiple: false });
    if (typeof selected !== "string") {
      return;
    }
    setLibrarySaving(true);
    setLibraryError(null);
    setLibraryResult(null);
    try {
      setLibraryResult(await onLibraryMigrate(selected));
    } catch (reason) {
      setLibraryError(formatTorbenError(reason));
    } finally {
      setLibrarySaving(false);
    }
  }

  const shellManaged = shellIntegration.state === "managed";
  const shellExternal = shellIntegration.state === "external";
  const shellStateLabel = t(`settingsPage.shellState.${shellIntegration.state}`);
  const shellPath = localizePlatformDataPath(
    shellIntegration.shimPath,
    t("settingsPage.platformDataDirectory"),
  );
  const managedLibraryPath = localizePlatformDataPath(
    managedLibrary.path,
    t("settingsPage.platformDataDirectory"),
  );
  const migratedLibraryPath = libraryResult
    ? localizePlatformDataPath(libraryResult.currentPath, t("settingsPage.platformDataDirectory"))
    : null;
  const updateBusy = matchesUpdateState(updateStatus.state, "checking", "installing");
  const updateStateLabel = t(`settingsPage.updateState.${updateStatus.state}`);

  return (
    <div className="page-stack">
      <PageHeader
        description={t("settingsPage.description")}
        eyebrow={t("settingsPage.eyebrow")}
        title={t("settingsPage.title")}
      />
      <div className="settings-grid">
        <Card>
          <div className="section-heading">
            <div>
              <span className="eyebrow">{t("settingsPage.appearance")}</span>
              <h2>{t("settingsPage.interface")}</h2>
            </div>
            <Laptop size={18} />
          </div>
          <SettingSelect
            disabled={saving}
            label={t("settingsPage.theme")}
            onChange={(theme) =>
              void changeSettings({ ...settings, theme: theme as UserSettings["theme"] })
            }
            options={[
              { label: t("settingsPage.system"), value: "system" },
              { label: t("settingsPage.light"), value: "light" },
              { label: t("settingsPage.dark"), value: "dark" },
            ]}
            value={settings.theme}
          />
          <SettingSelect
            disabled={saving}
            label={t("settingsPage.language")}
            onChange={(language) =>
              void changeSettings({
                ...settings,
                language: language as UserSettings["language"],
              })
            }
            options={[
              { label: t("settingsPage.system"), value: "system" },
              { label: t("settingsPage.english"), value: "en" },
              { label: t("settingsPage.simplifiedChinese"), value: "zh-CN" },
            ]}
            value={settings.language}
          />
          <SettingRow label={t("settingsPage.density")} value={t("settingsPage.comfortable")} />
          <div
            aria-live="polite"
            className={error ? "setting-status error-text" : "setting-status"}
            role={error ? "alert" : "status"}
          >
            {error ?? (saving ? t("settingsPage.saving") : "")}
          </div>
        </Card>
        <Card>
          <div className="section-heading">
            <div>
              <span className="eyebrow">{t("settingsPage.terminal")}</span>
              <h2>{t("settingsPage.shellIntegration")}</h2>
            </div>
            <TerminalSquare size={18} />
          </div>
          <SettingRow label={t("settingsPage.status")} value={shellStateLabel} />
          <div className="shell-path">
            <span>{t("settingsPage.shimPath")}</span>
            <code>{shellPath}</code>
          </div>
          <p className="settings-note">
            {shellExternal ? t("settingsPage.externalShellNote") : t("settingsPage.shellNote")}
          </p>
          <Button
            aria-label={
              shellManaged ? t("settingsPage.disableShell") : t("settingsPage.enableShell")
            }
            disabled={shellSaving || shellExternal}
            onClick={() => void changeShellIntegration(!shellManaged)}
            size="sm"
            variant={shellManaged ? "danger" : "secondary"}
          >
            <TerminalSquare size={14} />
            {shellSaving
              ? t("settingsPage.updatingShell")
              : shellManaged
                ? t("settingsPage.disableShell")
                : shellIntegration.state === "outdated"
                  ? t("settingsPage.repairShell")
                  : t("settingsPage.enableShell")}
          </Button>
          <div
            aria-live="polite"
            className={shellError ? "setting-status error-text" : "setting-status"}
            role={shellError ? "alert" : "status"}
          >
            {shellError ??
              (shellIntegration.newTerminalRequired ? t("settingsPage.newTerminal") : "")}
          </div>
        </Card>
        <Card>
          <div className="section-heading">
            <div>
              <span className="eyebrow">{t("settingsPage.storage")}</span>
              <h2>{t("settingsPage.managedLibrary")}</h2>
            </div>
            <Database size={18} />
          </div>
          <SettingRow label={t("settingsPage.applicationLibrary")} value={managedLibraryPath} />
          <SettingRow
            label={t("settingsPage.librarySize")}
            value={`${managedLibrary.bytesUsed.toLocaleString(
              translation.resolvedLanguage ?? translation.language,
            )} B`}
          />
          <SettingRow
            label={t("settingsPage.downloadCache")}
            value={t("settingsPage.automaticCleanup")}
          />
          <p className="settings-note">{t("settingsPage.migrationNote")}</p>
          <Button
            disabled={librarySaving || !onLibraryMigrate}
            onClick={() => void chooseLibraryTarget()}
            size="sm"
            variant="secondary"
          >
            <Wrench size={14} />
            {librarySaving ? t("settingsPage.migratingLibrary") : t("settingsPage.migrateLibrary")}
          </Button>
          <div
            aria-live="polite"
            className={
              libraryError
                ? "setting-status error-text"
                : libraryResult?.sourceCleanupPending
                  ? "setting-status warning-text"
                  : "setting-status"
            }
            role={libraryError ? "alert" : "status"}
          >
            {libraryError ??
              (libraryResult && migratedLibraryPath
                ? t(
                    libraryResult.sourceCleanupPending
                      ? "settingsPage.libraryMigrationCleanupPending"
                      : "settingsPage.libraryMigrationComplete",
                    { path: migratedLibraryPath },
                  )
                : "")}
          </div>
        </Card>
        <Card>
          <div className="section-heading">
            <div>
              <span className="eyebrow">{t("settingsPage.updates")}</span>
              <h2>{t("settingsPage.updatePolicy")}</h2>
            </div>
            <Clock3 size={18} />
          </div>
          <SettingRow label={t("settingsPage.currentVersion")} value={updater.currentVersion} />
          <SettingSelect
            disabled={saving}
            label={t("settingsPage.torbenApp")}
            onChange={(value) =>
              void changeSettings({
                ...settings,
                updates: { ...settings.updates, notifyTorbenApp: value === "enabled" },
              })
            }
            options={[
              { label: t("settingsPage.notifyOnly"), value: "enabled" },
              { label: t("settingsPage.disabled"), value: "disabled" },
            ]}
            value={settings.updates.notifyTorbenApp ? "enabled" : "disabled"}
          />
          <SettingSelect
            disabled={saving}
            label={t("settingsPage.managedApps")}
            onChange={(value) =>
              void changeSettings({
                ...settings,
                updates: { ...settings.updates, notifyManagedApps: value === "enabled" },
              })
            }
            options={[
              { label: t("settingsPage.notifyOnly"), value: "enabled" },
              { label: t("settingsPage.disabled"), value: "disabled" },
            ]}
            value={settings.updates.notifyManagedApps ? "enabled" : "disabled"}
          />
          <SettingRow
            label={t("settingsPage.backgroundService")}
            value={t("settingsPage.disabled")}
          />
          <SettingRow label={t("settingsPage.updateStatus")} value={updateStateLabel} />
          <p className="settings-note">
            {updateStatus.message ??
              (updater.configured
                ? t("settingsPage.signedUpdateNote")
                : t("settingsPage.unconfiguredUpdateNote"))}
          </p>
          {updateStatus.notes ? <p className="settings-note">{updateStatus.notes}</p> : null}
          {updateStatus.state === "installing" && updateStatus.progress !== null ? (
            <ProgressBar
              label={t("settingsPage.updateProgress")}
              value={updateStatus.progress * 100}
            />
          ) : null}
          <div className="row-actions update-actions">
            <Button
              disabled={!updater.configured || updateBusy}
              onClick={() => void onUpdateCheck()}
              size="sm"
              variant="secondary"
            >
              <RefreshCw size={14} />
              {updateStatus.state === "checking"
                ? t("settingsPage.checkingUpdates")
                : t("settingsPage.checkUpdates")}
            </Button>
            {updateStatus.state === "available" ? (
              <Button disabled={updateBusy} onClick={() => void onInstallTorbenUpdate()} size="sm">
                <ArrowDownToLine size={14} /> {t("settingsPage.installUpdate")}
              </Button>
            ) : null}
          </div>
        </Card>
        <Card>
          <div className="section-heading">
            <div>
              <span className="eyebrow">{t("settingsPage.privacy")}</span>
              <h2>{t("settingsPage.localFirst")}</h2>
            </div>
            <ShieldCheck size={18} />
          </div>
          <SettingRow label={t("settingsPage.account")} value={t("settingsPage.notRequired")} />
          <SettingRow label={t("settingsPage.cloudSync")} value={t("settingsPage.disabled")} />
          <SettingRow label={t("settingsPage.telemetry")} value={t("settingsPage.notCollected")} />
        </Card>
      </div>
    </div>
  );
}

function matchesUpdateState(
  state: TorbenUpdateStatus["state"],
  ...matches: TorbenUpdateStatus["state"][]
) {
  return matches.includes(state);
}

function SettingRow({ label, value }: { label: string; value: string }) {
  return (
    <div className="setting-row">
      <span>{label}</span>
      <strong>{value}</strong>
    </div>
  );
}

function SettingSelect({
  disabled,
  label,
  onChange,
  options,
  value,
}: {
  disabled: boolean;
  label: string;
  onChange: (value: string) => void;
  options: Array<{ label: string; value: string }>;
  value: string;
}) {
  return (
    <label className="setting-row setting-control">
      <span>{label}</span>
      <select
        aria-label={label}
        disabled={disabled}
        onChange={(event) => onChange(event.target.value)}
        value={value}
      >
        {options.map((option) => (
          <option key={option.value} value={option.value}>
            {option.label}
          </option>
        ))}
      </select>
    </label>
  );
}
function AppGlyph({ id }: { id: string }) {
  return <ApplicationIcon id={id} size={44} />;
}
function _applicationDisplayName(application: ApplicationDescriptor) {
  return application.id === "temurin" ? "Java" : application.displayName;
}
function displayPluginName(plugin: PluginSummary) {
  return plugin.id === "app.torben.plugin.temurin" ? "Java" : plugin.displayName;
}
function localizePlatformDataPath(path: string, platformDataDirectory: string) {
  const placeholder = "Platform data directory";
  return path === placeholder || path.startsWith(`${placeholder}/`)
    ? `${platformDataDirectory}${path.slice(placeholder.length)}`
    : path;
}
function formatTimestamp(timestamp: string, locale?: string) {
  const seconds = Number(timestamp);
  const date =
    Number.isFinite(seconds) && seconds > 0 ? new Date(seconds * 1000) : new Date(timestamp);
  return Number.isNaN(date.valueOf()) ? timestamp : date.toLocaleString(locale);
}
