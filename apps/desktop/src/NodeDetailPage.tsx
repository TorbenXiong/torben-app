import { Badge, Button, Card } from "@torben-app/ui";
import { ArrowDownToLine, Check, CircleAlert, RefreshCw, Trash2 } from "lucide-react";
import { Dialog } from "radix-ui";
import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  clearSelection,
  formatTorbenError,
  getVersions,
  installApp,
  onVersionCatalogUpdated,
  selectVersion,
  setShellIntegration,
  uninstallApp,
} from "./api";
import {
  activeInstallOperation,
  RuntimeOperationProgress,
} from "./components/RuntimeOperationProgress";
import type {
  InstallRecord,
  OperationEvent,
  SelectionRecord,
  ShellIntegrationStatus,
  VersionDescriptor,
} from "./types";

interface NodeVersionRow {
  version: string;
  channel: string;
  available?: VersionDescriptor;
  channelInfo?: VersionDescriptor;
  installed?: InstallRecord;
  selected: boolean;
}

function nodeMajor(version: string): string | undefined {
  return version.match(/^\d+/)?.[0];
}

function compareNodeVersions(left: string, right: string): number {
  const leftParts = left.split(".").map(Number);
  const rightParts = right.split(".").map(Number);
  for (let index = 0; index < Math.max(leftParts.length, rightParts.length); index += 1) {
    const difference = (leftParts[index] ?? 0) - (rightParts[index] ?? 0);
    if (difference !== 0) return difference;
  }
  return left.localeCompare(right);
}

function buildNodeVersionRows(
  versions: VersionDescriptor[],
  installed: InstallRecord[],
  selected: SelectionRecord[],
): NodeVersionRow[] {
  const channelInfo = new Map<string, VersionDescriptor>();
  for (const version of versions) {
    const major = nodeMajor(version.version);
    if (!major) continue;
    const current = channelInfo.get(major);
    if (!current || compareNodeVersions(version.version, current.version) > 0) {
      channelInfo.set(major, version);
    }
  }

  const rows = new Map<string, NodeVersionRow>();
  for (const available of versions) {
    const channel = nodeMajor(available.version);
    if (!channel) continue;
    rows.set(available.version, {
      version: available.version,
      channel,
      available,
      channelInfo: channelInfo.get(channel),
      selected: false,
    });
  }
  for (const record of installed.filter((record) => record.appId === "node")) {
    const channel = nodeMajor(record.version);
    if (!channel) continue;
    const current = rows.get(record.version);
    rows.set(record.version, {
      version: record.version,
      channel,
      available: current?.available,
      channelInfo: channelInfo.get(channel),
      installed: record,
      selected: false,
    });
  }
  const selectedVersion = selected.find((record) => record.appId === "node")?.version;
  return [...rows.values()]
    .map((row) => ({ ...row, selected: row.version === selectedVersion }))
    .sort((left, right) => compareNodeVersions(right.version, left.version));
}

export function NodeDetailPage({
  installed,
  selected = [],
  operations = [],
  onChanged,
  shellIntegration,
}: {
  installed: InstallRecord[];
  selected?: SelectionRecord[];
  operations?: OperationEvent[];
  onChanged: () => Promise<void>;
  shellIntegration?: ShellIntegrationStatus;
}) {
  const { t } = useTranslation();
  const [versions, setVersions] = useState<VersionDescriptor[]>([]);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState<Set<string>>(() => new Set());
  const [error, setError] = useState<string | null>(null);
  async function selectPrimary(version: string) {
    await selectVersion("node", version);
    if (
      shellIntegration &&
      (shellIntegration.state === "disabled" || shellIntegration.state === "outdated")
    ) {
      await setShellIntegration(true);
    }
  }
  const readVersions = useCallback(async (showLoading: boolean) => {
    if (showLoading) setLoading(true);
    setError(null);
    try {
      setVersions(await getVersions("node"));
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
    void onVersionCatalogUpdated((appId) => {
      if (appId === "node") void readVersions(false);
    })
      .then((stop) => {
        if (disposed) stop();
        else unlisten = stop;
      })
      .catch(() => undefined);
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [readVersions]);

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

  const rows = buildNodeVersionRows(versions, installed, selected);
  const selectedVersion = selected.find((record) => record.appId === "node")?.version;
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
                onClick={() => void run("clear-selection", () => clearSelection("node"))}
                size="sm"
                variant="secondary"
              >
                {t("nodePage.clearSelection")}
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
              {rows.length === 0 ? (
                <p className="version-catalog-status">{t("runtimePage.catalogUpdating")}</p>
              ) : null}
              {rows.map((row) => {
                const installAction = `install:${row.version}`;
                const installEvent = activeInstallOperation(operations, "node", row.version);
                const installing = busy.has(installAction) || Boolean(installEvent);
                return (
                  <div className="version-row runtime-version-row" key={row.version}>
                    <div className="version-main">
                      <strong>Node.js {row.channel}</strong>
                      {row.channelInfo?.ltsName ? (
                        <Badge tone="accent">LTS · {row.channelInfo.ltsName}</Badge>
                      ) : (
                        <Badge tone="neutral">Current</Badge>
                      )}
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
                              installApp("node", row.available?.version ?? ""),
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
                            {t("runtimePage.select")}
                          </Button>
                        )
                      ) : null}
                      {row.installed ? (
                        <Dialog.Root>
                          <Dialog.Trigger asChild>
                            <Button
                              disabled={busy.has(`uninstall:${row.installed?.version}`)}
                              size="sm"
                              variant="danger"
                            >
                              <Trash2 size={14} /> {t("runtimePage.uninstall")}
                            </Button>
                          </Dialog.Trigger>
                          <Dialog.Portal>
                            <Dialog.Overlay className="dialog-overlay" />
                            <Dialog.Content className="dialog-content">
                              <Dialog.Title>
                                {t("runtimePage.uninstallTitle", {
                                  app: "Node.js",
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
                                      void run(`uninstall:${row.installed?.version}`, async () => {
                                        if (row.selected) await clearSelection("node");
                                        await uninstallApp("node", row.installed?.version ?? "");
                                      })
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
                      event={installEvent}
                      pending={installing}
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
