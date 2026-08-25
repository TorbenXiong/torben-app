import { open } from "@tauri-apps/plugin-dialog";
import { Badge, Button, Card, EmptyState, PageHeader, ProgressBar } from "@torben-app/ui";
import {
  Activity,
  ArrowDownToLine,
  ArrowRight,
  Check,
  CheckCircle2,
  ChevronRight,
  CircleAlert,
  Clock3,
  Database,
  ExternalLink,
  FolderArchive,
  HardDrive,
  Laptop,
  PackageCheck,
  PlugZap,
  RefreshCw,
  Search,
  ShieldCheck,
  Sparkles,
  TerminalSquare,
  Trash2,
  Wrench,
} from "lucide-react";
import { Dialog } from "radix-ui";
import { type ReactNode, useCallback, useEffect, useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { Link } from "react-router";
import {
  applyManagedUpdate,
  cancelOperation,
  checkManagedUpdates,
  clearSelection,
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
  planManagedToPackageMigration,
  planPackageToManagedMigration,
  planSourceMigration,
  planSourceOperation,
  refreshOfficialPluginRegistry,
  selectVersion,
  setManagedAutoUpdate,
  setPluginEnabled,
  setShellIntegration,
  uninstallApp,
  updateSettings,
} from "./api";
import i18n from "./i18n";
import type {
  ApplicationDescriptor,
  DashboardSnapshot,
  DesktopUpdaterConfiguration,
  DoctorCheck,
  InstallRecord,
  ManagedLibraryMigrationResult,
  ManagedLibraryStatus,
  ManagedToPackageMigrationPlan,
  ManagedToPackageMigrationResult,
  ManagedUpdateCandidate,
  ManagedUpdateCheck,
  ManagedUpdateResult,
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

export function OverviewPage({ snapshot }: { snapshot: DashboardSnapshot }) {
  const { t } = useTranslation();
  const passing = snapshot.doctor.filter((check) => check.healthy).length;
  const attention = snapshot.doctor.length - passing;
  const activePlugins = snapshot.plugins.filter((plugin) => plugin.enabled).length;
  const recent = latestOperationEvents(snapshot.operations).slice(0, 4);
  return (
    <div className="page-stack">
      <PageHeader
        description={t("overviewPage.description")}
        eyebrow={t("overviewPage.eyebrow")}
        title={t("overviewPage.title")}
        actions={
          <Button asChild>
            <Link to="/catalog">
              {t("overviewPage.browseCatalog")} <ArrowRight size={15} />
            </Link>
          </Button>
        }
      />

      <section className="metric-grid">
        <Metric
          icon={<PackageCheck />}
          label={t("overviewPage.managedInstalls")}
          value={String(snapshot.installed.length)}
          detail={t("overviewPage.acrossSources")}
        />
        <Metric
          icon={<PlugZap />}
          label={t("overviewPage.activePlugins")}
          value={String(activePlugins)}
          detail={t("overviewPage.availableLocally", { count: snapshot.plugins.length })}
        />
        <Metric
          icon={<ShieldCheck />}
          label={t("overviewPage.healthChecks")}
          value={`${passing}/${snapshot.doctor.length}`}
          detail={
            attention === 0
              ? t("overviewPage.coreReady")
              : t("overviewPage.checksNeedAttention", { count: attention })
          }
          tone={attention === 0 ? "positive" : "warning"}
        />
        <Metric
          icon={<HardDrive />}
          label={t("overviewPage.storage")}
          value={t("overviewPage.local")}
          detail={t("overviewPage.noCloud")}
        />
      </section>

      <section className="split-grid">
        <Card className="feature-card">
          <div className="section-heading">
            <div>
              <span className="eyebrow">{t("overviewPage.milestone")}</span>
              <h2>{t("overviewPage.nodeJourney")}</h2>
            </div>
            <Badge tone="accent">{t("overviewPage.readyToExplore")}</Badge>
          </div>
          <p className="muted-copy">{t("overviewPage.nodeDescription")}</p>
          <div className="feature-visual">
            <div className="runtime-orb">JS</div>
            <div className="feature-lines">
              <span>
                <Check size={14} /> {t("overviewPage.officialMetadata")}
              </span>
              <span>
                <Check size={14} /> {t("overviewPage.transactionalInstall")}
              </span>
              <span>
                <Check size={14} /> node · npm · npx
              </span>
            </div>
          </div>
          <Button asChild variant="secondary">
            <Link to="/catalog/node">
              {t("overviewPage.openNode")} <ChevronRight size={15} />
            </Link>
          </Button>
        </Card>

        <Card>
          <div className="section-heading">
            <div>
              <span className="eyebrow">{t("overviewPage.activity")}</span>
              <h2>{t("overviewPage.recentOperations")}</h2>
            </div>
            <Button asChild size="sm" variant="ghost">
              <Link to="/tasks">{t("overviewPage.viewAll")}</Link>
            </Button>
          </div>
          {recent.length ? (
            <div className="activity-list">
              {recent.map((event) => (
                <OperationRow event={event} key={`${event.operationId}-${event.sequence}`} />
              ))}
            </div>
          ) : (
            <EmptyState
              description={t("overviewPage.emptyDescription")}
              title={t("overviewPage.emptyTitle")}
            />
          )}
        </Card>
      </section>
    </div>
  );
}

function Metric({
  icon,
  label,
  value,
  detail,
  tone,
}: {
  icon: ReactNode;
  label: string;
  value: string;
  detail: string;
  tone?: "positive" | "warning";
}) {
  return (
    <Card className="metric-card">
      <div className="metric-icon">{icon}</div>
      <div className="metric-value">{value}</div>
      <div className="metric-label">{label}</div>
      <div className={tone ? `metric-detail ${tone}` : "metric-detail"}>{detail}</div>
    </Card>
  );
}

export function CatalogPage({ applications }: { applications: ApplicationDescriptor[] }) {
  const { t } = useTranslation();
  const [query, setQuery] = useState("");
  const filtered = useMemo(() => {
    const normalized = query.trim().toLowerCase();
    return applications.filter((application) => {
      const localizedSummary = t(`catalogPage.summaries.${application.id}`, {
        defaultValue: application.summary,
      });
      const localizedCategories = application.categories.map((category) =>
        t(`catalogPage.categories.${category.toLowerCase()}`, { defaultValue: category }),
      );
      return [
        application.id,
        application.displayName,
        application.summary,
        localizedSummary,
        ...application.categories,
        ...localizedCategories,
      ]
        .join(" ")
        .toLowerCase()
        .includes(normalized);
    });
  }, [applications, query, t]);

  return (
    <div className="page-stack">
      <PageHeader
        description={t("catalogPage.description")}
        eyebrow={t("catalogPage.eyebrow")}
        title={t("catalogPage.title")}
      />
      <div className="catalog-toolbar">
        <label className="search-field">
          <Search size={15} />
          <input
            aria-label={t("catalogPage.searchLabel")}
            onChange={(event) => setQuery(event.target.value)}
            placeholder={t("catalogPage.searchPlaceholder")}
            value={query}
          />
        </label>
        <span className="result-count">
          {t("catalogPage.resultCount", { count: filtered.length })}
        </span>
      </div>
      <div className="catalog-grid">
        {filtered.map((application) => {
          const available = application.capabilities.length > 0;
          return (
            <Card className="app-card" key={application.id}>
              <div className={`app-icon app-icon-${application.id}`}>
                {appMonogram(application.id)}
              </div>
              <div className="app-card-body">
                <div className="app-card-title">
                  <h2>{application.displayName}</h2>
                  {available ? (
                    <Badge tone="positive">{t("common.available")}</Badge>
                  ) : (
                    <Badge>{t("catalogPage.planned")}</Badge>
                  )}
                </div>
                <p>
                  {t(`catalogPage.summaries.${application.id}`, {
                    defaultValue: application.summary,
                  })}
                </p>
                <div className="tag-row">
                  {application.categories.map((category) => (
                    <span key={category}>
                      {t(`catalogPage.categories.${category.toLowerCase()}`, {
                        defaultValue: category,
                      })}
                    </span>
                  ))}
                </div>
              </div>
              {available ? (
                <Button asChild size="sm" variant="secondary">
                  <Link to={`/catalog/${application.id}`}>
                    {t("catalogPage.manage")} <ChevronRight size={14} />
                  </Link>
                </Button>
              ) : (
                <Button disabled size="sm" variant="ghost">
                  {t("catalogPage.laterMilestone")}
                </Button>
              )}
            </Card>
          );
        })}
      </div>
    </div>
  );
}

export function NodeDetailPage({
  installed,
  onChanged,
}: {
  installed: InstallRecord[];
  onChanged: () => Promise<void>;
}) {
  return <RuntimeDetailPage appId="node" installed={installed} onChanged={onChanged} />;
}

export function TemurinDetailPage({
  installed,
  onChanged,
}: {
  installed: InstallRecord[];
  onChanged: () => Promise<void>;
}) {
  return <RuntimeDetailPage appId="temurin" installed={installed} onChanged={onChanged} />;
}

export function PythonDetailPage({
  installed,
  onChanged,
}: {
  installed: InstallRecord[];
  onChanged: () => Promise<void>;
}) {
  return <RuntimeDetailPage appId="python" installed={installed} onChanged={onChanged} />;
}

export function GitDetailPage({
  installed,
  onChanged,
}: {
  installed: InstallRecord[];
  onChanged: () => Promise<void>;
}) {
  return <RuntimeDetailPage appId="git" installed={installed} onChanged={onChanged} />;
}

export function VsCodeDetailPage({
  installed,
  onChanged,
}: {
  installed: InstallRecord[];
  onChanged: () => Promise<void>;
}) {
  return <RuntimeDetailPage appId="vscode" installed={installed} onChanged={onChanged} />;
}

export function CodexDetailPage({
  installed,
  onChanged,
}: {
  installed: InstallRecord[];
  onChanged: () => Promise<void>;
}) {
  return <RuntimeDetailPage appId="codex" installed={installed} onChanged={onChanged} />;
}

function RuntimeDetailPage({
  appId,
  installed,
  onChanged,
}: {
  appId: "node" | "temurin" | "python" | "git" | "vscode" | "codex";
  installed: InstallRecord[];
  onChanged: () => Promise<void>;
}) {
  const { t } = useTranslation();
  const [versions, setVersions] = useState<VersionDescriptor[]>([]);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const refreshVersions = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      setVersions(await getVersions(appId));
    } catch (reason) {
      setError(formatTorbenError(reason));
    } finally {
      setLoading(false);
    }
  }, [appId]);

  useEffect(() => {
    void refreshVersions();
  }, [refreshVersions]);

  async function install(version: string) {
    setBusy(version);
    setError(null);
    try {
      await installApp(appId, version);
      await onChanged();
    } catch (reason) {
      setError(formatTorbenError(reason));
    } finally {
      setBusy(null);
    }
  }

  const installedVersions = new Set(
    installed.filter((record) => record.appId === appId).map((record) => record.version),
  );
  const temurin = appId === "temurin";
  const python = appId === "python";
  const git = appId === "git";
  const vscode = appId === "vscode";
  const codex = appId === "codex";
  const displayName = temurin
    ? "Eclipse Temurin"
    : python
      ? "Python"
      : git
        ? "Git"
        : vscode
          ? "Visual Studio Code"
          : codex
            ? "Codex CLI"
            : "Node.js";
  return (
    <div className="page-stack">
      <div className="detail-hero">
        <div className={`app-icon app-icon-${appId} detail-icon`}>
          {temurin ? "J" : python ? "Py" : git ? "G" : vscode ? "<>" : codex ? "AI" : "JS"}
        </div>
        <div>
          <span className="eyebrow">{t(`runtimePage.apps.${appId}.eyebrow`)}</span>
          <h1>{displayName}</h1>
          <p>{t(`runtimePage.apps.${appId}.description`)}</p>
        </div>
        <div className="detail-trust">
          <ShieldCheck size={15} /> {t(`runtimePage.apps.${appId}.trust`)}
        </div>
      </div>
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
            <Button
              aria-label={t("runtimePage.refreshVersions", { app: displayName })}
              disabled={loading}
              onClick={() => void refreshVersions()}
              size="icon"
              variant="ghost"
            >
              <RefreshCw className={loading ? "spin" : undefined} size={15} />
            </Button>
          </div>
          {loading ? (
            <div className="skeleton-list">
              <span />
              <span />
              <span />
            </div>
          ) : (
            <div className="version-list">
              {versions.slice(0, 12).map((version) => {
                const isInstalled = installedVersions.has(version.version);
                return (
                  <div className="version-row" key={version.version}>
                    <div className="version-main">
                      <strong>v{version.version}</strong>
                      {version.ltsName ? (
                        <Badge tone="accent">LTS · {version.ltsName}</Badge>
                      ) : python || git || vscode || codex ? (
                        <Badge tone="accent">{t("common.stable")}</Badge>
                      ) : (
                        <Badge>{t("common.current")}</Badge>
                      )}
                      {version.recommended ? (
                        <span className="recommended">
                          <Sparkles size={12} /> {t("runtimePage.recommended")}
                        </span>
                      ) : null}
                    </div>
                    <span className="release-date">{version.releasedAt.slice(0, 10)}</span>
                    {isInstalled ? (
                      <Button disabled size="sm" variant="ghost">
                        <Check size={14} /> {t("runtimePage.installed")}
                      </Button>
                    ) : (
                      <Button
                        disabled={Boolean(busy)}
                        onClick={() => void install(version.version)}
                        size="sm"
                        variant="secondary"
                      >
                        {busy === version.version ? (
                          <RefreshCw className="spin" size={14} />
                        ) : (
                          <ArrowDownToLine size={14} />
                        )}{" "}
                        {t("common.install")}
                      </Button>
                    )}
                  </div>
                );
              })}
            </div>
          )}
        </Card>
        <div className="side-stack">
          <Card className="info-card">
            <TerminalSquare size={18} />
            <div>
              <strong>{t("runtimePage.terminalCommands")}</strong>
              <p>{t(`runtimePage.apps.${appId}.commands`)}</p>
            </div>
          </Card>
          <Card className="info-card">
            <FolderArchive size={18} />
            <div>
              <strong>{t("runtimePage.transactionalStorage")}</strong>
              <p>{t("runtimePage.transactionalDescription")}</p>
            </div>
          </Card>
          <Card className="info-card">
            <ShieldCheck size={18} />
            <div>
              <strong>{t("runtimePage.sourceOwnership")}</strong>
              <p>{t("runtimePage.sourceOwnershipDescription", { app: displayName })}</p>
            </div>
          </Card>
        </div>
      </div>
    </div>
  );
}

export function InstalledPage({
  records,
  external,
  selected,
  onChanged,
  updates = { checkedApps: 0, candidates: [], warnings: [] },
  settings,
  onCheckUpdates = checkManagedUpdates,
  onApplyUpdate = applyManagedUpdate,
  onAutoUpdateChange = setManagedAutoUpdate,
  onSettingsChanged,
}: {
  records: InstallRecord[];
  external: InstallRecord[];
  selected: SelectionRecord[];
  onChanged: () => Promise<void>;
  updates?: ManagedUpdateCheck;
  settings?: UserSettings;
  onCheckUpdates?: () => Promise<ManagedUpdateCheck>;
  onApplyUpdate?: (candidate: ManagedUpdateCandidate) => Promise<ManagedUpdateResult>;
  onAutoUpdateChange?: (appId: string, enabled: boolean) => Promise<UserSettings>;
  onSettingsChanged?: (settings: UserSettings) => void;
}) {
  const { t } = useTranslation();
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [checkingUpdates, setCheckingUpdates] = useState(false);
  const selectedVersions = new Map(
    selected.map((selection) => [selection.appId, selection.version]),
  );
  async function select(record: InstallRecord) {
    setBusy(`${record.appId}@${record.version}`);
    setError(null);
    try {
      await selectVersion(record.appId, record.version);
      await onChanged();
    } catch (reason) {
      setError(formatTorbenError(reason));
    } finally {
      setBusy(null);
    }
  }
  async function clear(appId: string) {
    setBusy(`${appId}@none`);
    setError(null);
    try {
      await clearSelection(appId);
      await onChanged();
    } catch (reason) {
      setError(formatTorbenError(reason));
    } finally {
      setBusy(null);
    }
  }
  async function remove(record: InstallRecord) {
    setBusy(`${record.appId}@${record.version}`);
    setError(null);
    try {
      await uninstallApp(record.appId, record.version);
      await onChanged();
    } catch (reason) {
      setError(formatTorbenError(reason));
    } finally {
      setBusy(null);
    }
  }
  async function refreshUpdates() {
    setCheckingUpdates(true);
    setError(null);
    try {
      await onCheckUpdates();
    } catch (reason) {
      setError(formatTorbenError(reason));
    } finally {
      setCheckingUpdates(false);
    }
  }
  async function applyUpdate(candidate: ManagedUpdateCandidate) {
    const key = `update-${candidate.appId}-${candidate.channel}`;
    setBusy(key);
    setError(null);
    try {
      await onApplyUpdate(candidate);
      await onChanged();
      await onCheckUpdates();
    } catch (reason) {
      setError(formatTorbenError(reason));
    } finally {
      setBusy(null);
    }
  }
  async function changeAutoUpdate(appId: string, enabled: boolean) {
    const key = `auto-${appId}`;
    setBusy(key);
    setError(null);
    try {
      const next = await onAutoUpdateChange(appId, enabled);
      onSettingsChanged?.(next);
      await onCheckUpdates();
    } catch (reason) {
      setError(formatTorbenError(reason));
    } finally {
      setBusy(null);
    }
  }
  return (
    <div className="page-stack">
      <PageHeader
        description={t("installedPage.description")}
        eyebrow={t("installedPage.eyebrow")}
        title={t("installedPage.title")}
        actions={
          <Button
            disabled={checkingUpdates || Boolean(busy) || records.length === 0}
            onClick={() => void refreshUpdates()}
            variant="secondary"
          >
            <RefreshCw size={15} />
            {checkingUpdates ? t("installedPage.checkingUpdates") : t("installedPage.checkUpdates")}
          </Button>
        }
      />
      {error ? (
        <div className="error-banner" role="alert">
          {error}
        </div>
      ) : null}
      {updates.warnings.map((warning) => (
        <div className="error-banner" key={`${warning.appId}-${warning.code}`} role="status">
          [{warning.code}] {warning.appId}: {warning.message}
          {warning.remediation ? ` ${warning.remediation}` : ""}
        </div>
      ))}
      {updates.candidates.length ? (
        <Card className="managed-update-list">
          <div className="section-heading">
            <div>
              <span className="eyebrow">{t("installedPage.updates")}</span>
              <h2>{t("installedPage.managedUpdates")}</h2>
              <p>{t("installedPage.updateDescription")}</p>
            </div>
            <Badge tone="accent">
              {t("installedPage.availableCount", { count: updates.candidates.length })}
            </Badge>
          </div>
          {updates.candidates.map((candidate) => {
            const automatic =
              settings?.updates.automaticallyUpdateApps.includes(candidate.appId) ??
              candidate.automatic;
            const updateKey = `update-${candidate.appId}-${candidate.channel}`;
            const autoKey = `auto-${candidate.appId}`;
            return (
              <div className="managed-update-row" key={`${candidate.appId}-${candidate.channel}`}>
                <span className={`app-icon small app-icon-${candidate.appId}`}>
                  {appMonogram(candidate.appId)}
                </span>
                <div>
                  <strong>{candidate.appId}</strong>
                  <p>
                    {candidate.installedVersion} → {candidate.availableVersion} ·{" "}
                    {t("installedPage.channel", { channel: candidate.channel })}
                  </p>
                  {candidate.selectedVersion ? <p>{t("installedPage.selectedMoves")}</p> : null}
                </div>
                <span className="row-actions">
                  <Button
                    aria-label={t("installedPage.autoUpdateAria", {
                      action: automatic ? t("pluginsPage.disable") : t("common.enable"),
                      app: candidate.appId,
                    })}
                    disabled={Boolean(busy)}
                    onClick={() => void changeAutoUpdate(candidate.appId, !automatic)}
                    size="sm"
                    variant="ghost"
                  >
                    {busy === autoKey
                      ? t("installedPage.saving")
                      : automatic
                        ? t("installedPage.autoOn")
                        : t("installedPage.autoOff")}
                  </Button>
                  <Button
                    aria-label={t("installedPage.updateAria", {
                      app: candidate.appId,
                      version: candidate.availableVersion,
                    })}
                    disabled={Boolean(busy)}
                    onClick={() => void applyUpdate(candidate)}
                    size="sm"
                  >
                    <ArrowDownToLine size={14} />
                    {busy === updateKey ? t("common.updating") : t("installedPage.update")}
                  </Button>
                </span>
              </div>
            );
          })}
        </Card>
      ) : null}
      {records.length || external.length ? (
        <Card className="table-card">
          <div className="data-table table-header">
            <span>{t("installedPage.application")}</span>
            <span>{t("installedPage.version")}</span>
            <span>{t("installedPage.source")}</span>
            <span>{t("installedPage.health")}</span>
            <span />
          </div>
          {records.map((record) => {
            const isSelected = selectedVersions.get(record.appId) === record.version;
            const isPackageManager = record.scope === "package_manager";
            const operationKey = `${record.appId}@${record.version}`;
            return (
              <div className="data-table" key={operationKey}>
                <span className="table-app">
                  <span className={`app-icon small app-icon-${record.appId}`}>
                    {appMonogram(record.appId)}
                  </span>
                  <strong>{record.appId}</strong>
                </span>
                <code>{record.version}</code>
                <span className="source-cell">
                  {record.sourceId}
                  {isPackageManager ? (
                    <Badge tone="warning">{t("installedPage.packageManager")}</Badge>
                  ) : null}
                </span>
                <Badge tone="positive">
                  {record.health === "healthy" ? t("common.healthy") : record.health}
                </Badge>
                <span className="row-actions">
                  {isPackageManager ? (
                    <Button asChild size="sm" variant="secondary">
                      <Link to="/diagnostics">{t("installedPage.manageSource")}</Link>
                    </Button>
                  ) : isSelected ? (
                    <>
                      <Badge tone="accent">{t("installedPage.selected")}</Badge>
                      <Button
                        disabled={Boolean(busy)}
                        onClick={() => void clear(record.appId)}
                        size="sm"
                        variant="ghost"
                      >
                        {busy === `${record.appId}@none`
                          ? t("installedPage.clearing")
                          : t("installedPage.clear")}
                      </Button>
                    </>
                  ) : (
                    <Button
                      disabled={Boolean(busy)}
                      onClick={() => void select(record)}
                      size="sm"
                      variant="secondary"
                    >
                      {busy === operationKey
                        ? t("installedPage.selecting")
                        : t("installedPage.use")}
                    </Button>
                  )}
                  {!isPackageManager ? (
                    <Dialog.Root>
                      <Dialog.Trigger asChild>
                        <Button
                          aria-label={t("installedPage.uninstallAria", {
                            app: record.appId,
                            version: record.version,
                          })}
                          disabled={isSelected || Boolean(busy)}
                          size="icon"
                          title={isSelected ? t("installedPage.clearBeforeUninstall") : undefined}
                          variant="ghost"
                        >
                          <Trash2 size={15} />
                        </Button>
                      </Dialog.Trigger>
                      <Dialog.Portal>
                        <Dialog.Overlay className="dialog-overlay" />
                        <Dialog.Content className="dialog-content">
                          <Dialog.Title>
                            {t("installedPage.uninstallTitle", {
                              app: record.appId,
                              version: record.version,
                            })}
                          </Dialog.Title>
                          <Dialog.Description>
                            {t("installedPage.uninstallDescription")}
                          </Dialog.Description>
                          <div className="dialog-actions">
                            <Dialog.Close asChild>
                              <Button variant="ghost">{t("common.cancel")}</Button>
                            </Dialog.Close>
                            <Dialog.Close asChild>
                              <Button onClick={() => void remove(record)} variant="danger">
                                {busy === operationKey
                                  ? t("installedPage.uninstalling")
                                  : t("installedPage.uninstall")}
                              </Button>
                            </Dialog.Close>
                          </div>
                        </Dialog.Content>
                      </Dialog.Portal>
                    </Dialog.Root>
                  ) : null}
                </span>
              </div>
            );
          })}
          {external.map((record) => (
            <div className="data-table" key={`external-${record.installPath}`}>
              <span className="table-app">
                <span className={`app-icon small app-icon-${record.appId}`}>
                  {appMonogram(record.appId)}
                </span>
                <strong>{record.appId}</strong>
              </span>
              <code>{record.version}</code>
              <span>{record.sourceId}</span>
              <Badge>{record.health === "healthy" ? t("common.healthy") : record.health}</Badge>
              <span className="row-actions">
                <Badge tone="warning">{t("installedPage.readOnly")}</Badge>
              </span>
            </div>
          ))}
        </Card>
      ) : (
        <EmptyState
          description={t("installedPage.emptyDescription")}
          title={t("installedPage.emptyTitle")}
        />
      )}
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

export function TasksPage({
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
        description={t("tasksPage.description")}
        eyebrow={t("tasksPage.eyebrow")}
        title={t("tasksPage.title")}
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
        <EmptyState
          description={t("tasksPage.emptyDescription")}
          title={t("tasksPage.emptyTitle")}
        />
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
  const stateLabel = t(`tasksPage.state.${event.state}`);
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
          <ProgressBar label={t("tasksPage.progress", { phase: event.phase })} value={progress} />
        ) : null}
      </div>
      <div className="operation-meta">
        <Badge tone={stateTone}>{stateLabel}</Badge>
        <time>
          {formatTimestamp(event.timestamp, translation.resolvedLanguage ?? translation.language)}
        </time>
        {onCancel ? (
          <Button disabled={cancelRequested} onClick={onCancel} size="sm" variant="ghost">
            {cancelRequested ? t("tasksPage.cancelling") : t("tasksPage.cancel")}
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
  registry?: PluginRegistryStatus;
  onChanged: () => Promise<void>;
  chooseManifest?: () => Promise<string | null>;
  installManifest?: (manifestPath: string, developerMode: boolean) => Promise<PluginSummary>;
  installRegistryPlugin?: (pluginId: string, version?: string) => Promise<PluginSummary>;
  refreshRegistry?: () => Promise<PluginRegistryStatus>;
  loadSchemaPages?: (pluginId: string) => Promise<SchemaPage[]>;
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

export function PluginsPage({
  plugins,
  registry = {
    configured: false,
    sourceUrl: null,
    cachePath: "",
    sequence: null,
    generatedAt: null,
  },
  onChanged,
  chooseManifest = choosePluginManifest,
  installManifest = installPlugin,
  installRegistryPlugin = installOfficialPluginFromRegistry,
  refreshRegistry = refreshOfficialPluginRegistry,
  loadSchemaPages = getPluginSchemaPages,
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

  async function openSchemaPages(plugin: PluginSummary) {
    setBusy(`schema:${plugin.id}`);
    setError(null);
    setSchemaMessage(null);
    try {
      const pages = await loadSchemaPages(plugin.id);
      setSchemaPlugin(plugin);
      setSchemaPages(pages);
      setSelectedSchemaPage(pages[0]?.id ?? null);
      setSchemaValues(initialSchemaValues(pages));
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

  return (
    <div className="page-stack">
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
                <ul className="trust-checklist">
                  <li>{t("pluginsPage.developerRegistryWarning")}</li>
                  <li>{t("pluginsPage.developerTrustWarning")}</li>
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
      <Card className="registry-card">
        <div className="app-icon">
          <ShieldCheck size={22} />
        </div>
        <div className="registry-card-body">
          <div className="app-card-title">
            <h2>{t("pluginsPage.registryTitle")}</h2>
            <Badge tone={registry.configured ? "positive" : "warning"}>
              {registry.configured
                ? t("pluginsPage.configured")
                : t("pluginsPage.developmentBuild")}
            </Badge>
          </div>
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
        <div className="registry-actions">
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
      </Card>
      <div className="plugin-list">
        {plugins.map((plugin) => {
          const builtIn = plugin.origin === "built_in";
          const official = plugin.origin === "official_registry";
          const bundledAppId =
            plugin.id === "app.torben.plugin.temurin"
              ? "temurin"
              : plugin.id === "app.torben.plugin.python"
                ? "python"
                : plugin.id === "app.torben.plugin.git"
                  ? "git"
                  : plugin.id === "app.torben.plugin.vscode"
                    ? "vscode"
                    : plugin.id === "app.torben.plugin.codex"
                      ? "codex"
                      : "node";
          return (
            <Card className={`plugin-card${plugin.enabled ? "" : " is-disabled"}`} key={plugin.id}>
              <div className={builtIn ? `app-icon app-icon-${bundledAppId}` : "app-icon"}>
                {builtIn ? appMonogram(bundledAppId) : plugin.displayName.slice(0, 2).toUpperCase()}
              </div>
              <div>
                <div className="app-card-title">
                  <h2>{plugin.displayName}</h2>
                  <span className="plugin-badges">
                    <Badge tone={builtIn || official ? "positive" : "warning"}>
                      {builtIn
                        ? t("pluginsPage.builtIn")
                        : official
                          ? t("pluginsPage.officialRegistry")
                          : t("pluginsPage.sideloaded")}
                    </Badge>
                    <Badge>{plugin.enabled ? t("common.enabled") : t("common.disabled")}</Badge>
                  </span>
                </div>
                <p className="plugin-metadata">
                  {plugin.publisher} · v{plugin.version} ·{" "}
                  {t("pluginsPage.capabilityCount", { count: plugin.capabilities.length })}
                </p>
                <PluginPermissionList permissions={plugin.permissions} />
              </div>
              <div className="plugin-card-actions">
                {plugin.capabilities.includes("schema_ui") ? (
                  <Button
                    aria-label={t("pluginsPage.openPagesAria", { plugin: plugin.displayName })}
                    disabled={!plugin.enabled || Boolean(busy)}
                    onClick={() => void openSchemaPages(plugin)}
                    size="sm"
                  >
                    <Wrench size={14} />
                    {busy === `schema:${plugin.id}`
                      ? t("pluginsPage.opening")
                      : t("pluginsPage.open")}
                  </Button>
                ) : null}
                <Button
                  aria-label={
                    builtIn
                      ? t("pluginsPage.bundledAria", { plugin: plugin.displayName })
                      : t("pluginsPage.toggleAria", {
                          action: plugin.enabled ? t("pluginsPage.disable") : t("common.enable"),
                          plugin: plugin.displayName,
                        })
                  }
                  disabled={builtIn || Boolean(busy)}
                  onClick={() => void togglePlugin(plugin)}
                  size="sm"
                  variant="secondary"
                >
                  {busy === plugin.id
                    ? t("common.updating")
                    : builtIn
                      ? t("pluginsPage.bundled")
                      : plugin.enabled
                        ? t("pluginsPage.disable")
                        : t("common.enable")}
                </Button>
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
          if (!open) closeSchemaPages();
        }}
        open={schemaPlugin !== null}
      >
        <Dialog.Portal>
          <Dialog.Overlay className="dialog-overlay" />
          <Dialog.Content className="dialog-content schema-dialog">
            <Dialog.Title>
              {t("pluginsPage.pagesTitle", { plugin: schemaPlugin?.displayName ?? t("plugins") })}
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
      <div className="trust-note">
        <ShieldCheck size={17} />
        <div>
          <strong>{t("pluginsPage.trustedCode")}</strong>
          <p>{t("pluginsPage.trustedCodeDescription")}</p>
        </div>
      </div>
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
    currentVersion: "0.1.0",
    endpoint: "",
  },
  updateStatus = {
    state: "unconfigured",
    currentVersion: "0.1.0",
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
function appMonogram(id: string) {
  return (
    (
      { node: "JS", temurin: "J", python: "Py", git: "G", vscode: "<>", codex: "AI" } as Record<
        string,
        string
      >
    )[id] ?? id.slice(0, 2).toUpperCase()
  );
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
